//! Dukascopy execution — drives the JForex Java sidecar over JSON-lines stdio.
//!
//! Dukascopy's only trading API is JForex (Java), so unlike every HMAC-REST venue this
//! client owns a CHILD PROCESS (`java -jar jforex-bridge.jar`) instead of a socket or
//! FFI handle: commands go to child stdin, canonical [`Event`] envelopes come back on
//! stdout and are pumped into the core ingest (the same inbound shape as the WS
//! user-data pumps). See docs/superpowers/specs/2026-07-04-dukascopy-jforex-bridge-design.md.
//!
//! LIVE GATE: jar missing, `java` missing, login failure, or handshake timeout →
//! [`DukascopyError::Unavailable`] → the account is NOT RUNNING; its kind says whether a later start can pass
//! (only a failure to reach Dukascopy's servers can: `fatal_kind`). A child that dies later is
//! RESTARTED by the client itself (the next section); until it is back, every order is
//! rejected ("bridge unavailable: ...", it was NOT sent) and the reader thread's EOF drain ends every
//! already-in-flight order the sidecar had answered ("bridge died: ..."). An order it had NOT answered
//! may rest at the venue, so it is never rejected (decision 0303): the next sidecar reads the venue for it
//! ([`settle_unanswered`]), or the core's stuck-order ladder settles it. Either way no order is left untracked.
//! A panic in the thread that reads the sidecar ends that sidecar like a death ([`READER_PANICKED`]), and one in
//! the supervisor says on the account's link that nothing restarts a dead sidecar any more ([`SUPERVISOR_PANICKED`]).
//!
//! # ⚠ The sidecar is SUPERVISED: a dead sidecar comes back by itself, and says so while it is away
//!
//! A sidecar that dies after `ready` (JForex ends its session at a server shutdown, a network drop it cannot
//! reconnect, an OOM) used to be gone for good: the reader hit EOF, every later submit was rejected, and nothing
//! started another one, while the node still counted the account as running (measured 2026-10-26: the sidecar
//! died at 08:15:03 UTC on `JForex session disconnected` and was not started again for 2.5 hours, until a deploy
//! restarted the daemon). Now a supervisor thread, one per client, starts the next sidecar:
//!
//! * **Backoff**: the first restart waits `RESTART.initial`, each failed one doubles the wait up to `RESTART.max`
//!   (the exec lane's `reconnect` row of `vike_venues::venue_timings::DUKASCOPY`, which carries the reason), and it
//!   never gives up while the client lives: no tight loop, no hand step. A restarted sidecar that dies again before it has
//!   lived `RESTART.max` is a FLAP and counts as one more failed restart, so a venue that keeps cutting the session
//!   off is backed off like a refused login (only a sidecar that lived that long starts the waits over). A start that says nothing is cut at
//!   [`ATTEMPT_READY_TIMEOUT`] and a refused one (a `fatal`, a failed spawn, a silent death) counts as one failed restart;
//!   every one is logged WITH the sidecar's own reason.
//! * **One JForex session per login, always**: the next sidecar starts only after the dead one has been killed
//!   and REAPED, and a client that is detached or dropped (the account deactivated, the tier changed, a clean stop,
//!   an orphan reaped) ends the supervisor and kills a sidecar that was still logging in, so no second session ever
//!   shares the login and the loop outlives nothing. That is also why the loop reads no account row: the account
//!   being deactivated ends the CLIENT (`crates/vike-tradehub-core/src/node/hot.rs`'s `detach_account` closes its
//!   session, and the gatewayhub tears the adapter down), and the client ends the loop.
//! * **While it is away it is not healthy, and it says so by name**: [`ExecutionClientDukascopy::sidecar_state`] is
//!   `VenueLinkState::Down` (the shared link reading, `sidecar_down`), the death is one `error` line (`the JForex
//!   sidecar is DOWN`), a submit is rejected
//!   `bridge unavailable: the JForex sidecar is down (...) ... this order was NOT sent`, a cancel is an
//!   `OrderCancelRejected` and an amend an `OrderModifyRejected` with the same words, and the price lane's bus has
//!   no sidecar attached (it replays its wants to the next one). The state itself lives on the account's ONE cell
//!   (`vike_bridge_core::venue_link::venue_link` of the start's route key, written through `Shared::set_state`), which
//!   the gatewayhub's trade lane reads for every venue, so the lease's status says the path is down and `trade health`
//!   stops reading `RUNNING` as healthy.
//! * **What a new sidecar knows depends on its jar.** A jar that announces `adopt` ([`FEATURE_ADOPT`]) takes up, at its
//!   start, the orders the VENUE lists whose label names an order_client_id (`StrategyBridge.adoptOpenOrders`), sends a
//!   `position` line per instrument and one `adopted` census (`Envelope::Adopted`). The reader then lifts the
//!   `stranded` refusal for every adopted order client id (the new process can cancel, amend and hear it again), and reads a
//!   symbol the node's fills hold a position in and the census lists no order on as FLAT at the venue (a loud warn,
//!   never a write: the phantom, `crates/bridges/dukascopy/CLAUDE.md`). A jar that does not adopt starts with no memory
//!   of the orders the dead one placed (its label, fill-sequence and commission maps are in-process), so it can neither
//!   cancel, amend nor report a fill of an order that was resting across the death: a cancel or amend of such an order
//!   is refused by name here (`stranded`) instead of being skipped in silence by Java, and its fill is NOT reported (a
//!   position built that way shows only at the next fill's `position` line, as a loud `SizeMismatch`). Closing a
//!   position needs none of that memory: a market order nets against what the VENUE holds (`engine.getOrders`), and a
//!   closing fill's id (`<order id>:c:<order client id>`, `OrderMap.closeTradeId`) is restart-safe by construction.
//! * **A resting native STOP-LOSS or TAKE-PROFIT is kept across the death of a sidecar that adopts, and sent again**
//!   ([`hand_back_protections`]): the venue holds the price and the next sidecar TAKES IT OVER when the node's stop is
//!   sent to it again, so the node keeps believing in a protection the venue provably holds and learns when it does not
//!   (the stop sent again is refused, and ends `OrderCanceled` in the venue's words). ⚠ **One whose CANCEL was in flight
//!   at the death is not sent blind**: the order may still be open without the price, and the submit would set it again.
//!   The new sidecar reads the venue first ([`settle_cancels_in_flight`]): a price it still holds is taken over and
//!   cancelled again, one it no longer holds ends `OrderCanceled`, and nothing is set again. For a jar that does NOT adopt it
//!   **ends `OrderCanceled`, in words, at the death** ([`STOP_BRIDGE_DIED`], [`TAKE_PROFIT_BRIDGE_DIED`]), never
//!   `OrderRejected`: the core folds a rejection only from `Initialized`/`Submitted` and DROPS it on an `Accepted`
//!   order, which left the stop `Accepted` on the node for ever while the venue's stop-loss fired unheard. The
//!   cancel is the NODE's belief only: nothing is sent to the venue (a restart never cancels a resting order, and the
//!   stop-loss survives the sidecar, measured 2026-10-26), and the words say the venue may still hold it. A stop the
//!   sidecar had not yet answered is an order it never answered, like any other (the bullet on those, below).
//! * **So does a REDUCE-ONLY MARKET close the sidecar had taken up** ([`CLOSE_BRIDGE_DIED`]): a flatten, a margin
//!   call, a market exit, a liquidation. The sidecar answers one with `OrderAccepted` (the synthetic net-accept) before it
//!   asks the venue to close, so a death between that and the close's `ORDER_CLOSE_OK` left the close `Accepted` on the
//!   node for ever: the rejection was dropped, and `vike-core`'s `closes_in_flight` counted the dead close as cover, so
//!   the margin call and a mount budget's flatten sent nothing for a position that may still be open. The venue may or may
//!   not have executed it and nobody can say from here, which the words state; the order ends, and the next close is
//!   either served or refused by the venue in words (`reduce-only: no open long order to reduce`). It is ended at the
//!   death even when the jar adopts, unlike a market entry: a close places no order under its own label (it closes the
//!   orders facing it), so the next sidecar's read has nothing to find it by.
//! * **A plain MARKET entry the sidecar had acknowledged is READ, not ended in words, when the jar adopts**: the venue
//!   may have filled it and the fill died with the process, so it stays in flight (`Shared::unanswered`) and the next
//!   sidecar's venue read ends it by what the venue lists under its label ([`settle_unanswered`]): FILLED, its fill is
//!   booked once ([`book_read_fill`], the trade id the reconcile leg reports it by) and it ends filled; listed resting,
//!   the new sidecar took it up and reports its fill; not listed, it ends `OrderCanceled` ([`ENTRY_GONE`]). Ended in
//!   words at the death ([`ENTRY_BRIDGE_DIED`], NOTHING booked: a fill that went through is a position at the venue
//!   the node does not hold) only where no read can settle it: a jar that does not adopt or cannot be asked, or an
//!   entry a part of whose fill was already reported. A resting LIMIT the sidecar had accepted (its venue state is
//!   unknown by design) keeps the rejection, which the core drops on an accepted order (a jar that adopts takes it up
//!   again).
//! * **An order the sidecar had NOT answered is never ended at its death** (decision 0303: its submit may have reached
//!   the venue, so its outcome is unknown, and a rejection would make every reader stop tracking an order that may
//!   rest). It stays `Submitted`. When the dying jar adopts, it stays in flight (`Shared::unanswered`) and the next
//!   sidecar reads the venue for it by its order client id, the read [`settle_cancels_in_flight`] asks for: held (a
//!   resting order of ours, or a protection's price on a facing order) it is accepted, a protection by the new
//!   sidecar's take-over; FILLED under its label, it is accepted and its fill booked; not listed, it is left to the
//!   core's stuck-order ladder (its confirm, grace and last resort, decision 0171) and reconcile, as it is at once for a
//!   jar that does not adopt ([`settle_unanswered`]). It stays in flight across a restart that never says `ready` (a
//!   refused login), and a CLEAN STOP does not end it either: its submit was written all the same, so it is left
//!   `Submitted` for the ladder and the restart reconcile. An order the sidecar HAD acknowledged keeps the endings above:
//!   its write was accepted, and what died is its later report.
//! * **A venue's refusal of an order the sidecar had acknowledged is folded as `OrderCanceled`**, with the venue's
//!   words (`settle`): a `fill rejected` after the venue's `ORDER_SUBMIT_OK` was an `OrderRejected` the core dropped,
//!   and the order stayed `Accepted` for ever. A reduce-only close the venue could only partly take (it closes what
//!   is open and drops the rest) ends `OrderPartiallyFilled` then `OrderCanceled` in words, never `OrderFilled`.
//! * **The netting shadow survives the restart** ([`PositionsShadow`] is shared by every reader of the client): a shadow
//!   born empty at each sidecar would adopt the venue's position as its baseline and re-anchor the core's blend
//!   against a basis the core never folded.
//! * A start that FAILS (the first one, before any sidecar lived) is not supervised: it stays the live gate above.
//! * **While a sidecar lives the supervisor says, once a minute, the longest gap between the lines it wrote**
//!   (`link gap summary`, `vike_bridge_core::link_gap`; every stdout line is an event, and the sidecar writes no
//!   heartbeat there, so `since_last_ms` is the age of its last line of any kind): the evidence the idle limit below
//!   was measured from (the FX open of 2026-11-22, the row's `why`).
//! * **A sidecar that writes nothing for too long is WEDGED, and is killed and restarted like a dead one**
//!   ([`watch_liveness`], over [`LIVENESS`]: the exec lane's `keepalive_every` and `idle_threshold`). Its steady lines
//!   are the answers to the node's reconcile pass (three `recon` questions a pass, every `config.reconcile_interval_ms`),
//!   which `flags.reconcile_off` stops, so the supervisor asks the `recon` question ITSELF once the sidecar has been
//!   silent for the probe interval, under `crate::reconcile::query::PROBE_ID`, an id no reconcile asker is given, so the
//!   answer can never answer a pass's question (`ReconcileLink::deliver` matches by id). Past the idle limit, counted
//!   from its `ready` (the handshake's bounds govern the start), the sidecar is killed with a warn naming the silence
//!   and the limit; its reader's end says it DOWN in those words and the supervisor reaps it and starts the next one on
//!   the ladder above. A JForex session that ends is not this case (it ends the sidecar through `fatal`), so what the
//!   limit catches is a stuck JVM, strategy thread or stdio. A jar that does not announce `recon` gets no probe and no
//!   limit (one `info` line at its `ready`).
//!
//! # The HALT sentinel, on a client that inherits nothing
//!
//! Every venue whose `submit` delegates to `vike_bridge_core::exec_actor::ExecutionClientActor` gets the
//! operator kill switch for free. This one delegates to nothing — it owns a child process — so it
//! reads the sentinel itself, in [`ExecutionClientDukascopy::submit`], through the SAME two shared
//! functions every other enforcing client calls (`halt_admits_submit` for the verdict,
//! `HALT_REJECT_REASON` for the wording) so it cannot drift into its own idea of what a halt lets
//! out. `docs/ops/kill-switches.md` carried its absence for months, correctly labelled LATENT:
//! nothing in production started this venue, so the hazard was never a live order leaking past an
//! engaged switch — it was that somebody would wire the venue and inherit the silence with it.
//! Decision 0168 wired it, and `crate::adapter`'s `start` hands every client its halt path
//! (`with_halt_path`), so the switch is in force the day the venue is.
//!
//! # Every cancel and amend is answered, and an unknown outcome is never called a refusal
//!
//! JForex's refusal of a cancel or an amend reaches the node as the non-terminal `OrderCancelRejected` /
//! `OrderModifyRejected` in its words, with `order.venue_refused` (`StrategyBridge.java`: an `ORDER_CLOSE_REJECTED`, an
//! `IOrder.close()` that throws, a cancel of an order that no longer rests, a stop's price that could not be taken off).
//! A change JForex answers neither way within the sidecar's bound, and whose read-back at the bound cannot tell, comes as
//! an [`Envelope::Unsettled`] line: OUTCOME UNKNOWN (decision 0303), answered with the shared answer of an unknown
//! outcome ([`answer_unsettled`]) and settled by JForex's late `ORDER_CHANGED_OK` / `ORDER_CHANGED_REJECTED`, which the
//! sidecar sends as the answer, or by a venue read ([`settle_unsettled`]). Nothing is sent again.
//!
//! # ⚠ The DUAL-PUBLISH fill contract (break this and position/PnL silently desync)
//!
//! A fill arrives on the wire as TWO envelopes carrying the SAME fill, and both must reach the
//! core, in order: the BARE `Event::Fill` first — which the core `Account` folds into position and
//! realized PnL independently of any order — and then the wrapping `Event::OrderFilled` the order
//! FSM applies. Dropping either one leaves half the system right and the other half permanently
//! wrong, and neither half complains.
//!
//! The netting shadow (`PositionsShadow::fold_fill`, called from the reader thread) folds the **bare
//! lane ONLY**, for exactly the reason the `Account` does: the wrap carries the same fill, so
//! folding both double-counts. If you add a fill-consuming side effect to the reader loop, decide
//! which lane it belongs on before you write it — "both" is never the answer.
//!
//! # The price lane rides this same child
//!
//! The sidecar is ONE process per daemon, so live quotes for an instrument the engine was not mounted on
//! (any-symbol) cannot come from a second login: `crate::feed`' bus is attached to this client's stdin and
//! stdout by its reader thread (on `ready`, the `tick` envelopes, detach at EOF), and the data lane's stream asks for
//! symbols through it. This client sends no price command itself and a client nobody listens to ([`ExecutionClientDukascopy::spawn`])
//! behaves exactly as before. Writes to the child's stdin from two places go through one mutex, a whole line at a
//! time.
//!
//! # The `ready` balance is the engine's balance, once per start
//!
//! The venue has no reconcile leg, so the sidecar's `ready` line is the only place the engine can learn the
//! account's balance. The reader's first-`ready` arm hands it to `crate::account::ready_balance`, which builds ONE
//! `AccountState` wallet frame for a USD account on a USD-quoted mount (the start tells the client its mount in
//! `DukascopyConfig::ledger_mount`) and sends it after the handshake reply. ⚠ The figure is `IAccount.getBalance()`,
//! the vendor's PREVIOUS END-OF-DAY balance: fresh only at start, so realized PnL of the engine's own trades stays
//! out of its equity until the next start. That module's doc carries the gate, the residual and what the operator
//! sees when the frame is withheld; the gate `crates/vike-ops/tests/venues/wallet_frame_venues_gate.rs` holds the
//! "one frame per start" claim to this reader's bytes.
//!
//! # ⚠ The JNLP must BE a JNLP
//!
//! `DukascopyConfig::server` is handed to the sidecar as `DUKASCOPY_JNLP`, verbatim. It is the
//! `dukascopy demo login rest jnlp` address row, and the loader (`crate::config`'s
//! `load_dukascopy_config_from`) refuses a row that does not end in `.jnlp`: the old server
//! setting commonly held the WEB-PLATFORM login URL, which the sidecar hands to `IClient.connect`
//! as a JNLP and HANGS on — no error, no timeout distinguishable from a slow login. A mis-set row
//! is therefore a start refused by name, not a stall — do not "fix" that refusal by trusting the
//! value. There is no built-in JNLP to fall back to.
//!
//! # ⚠ The sidecar's HOME is the project's, not the operating-system user's
//!
//! The bundled JForex SDK writes its working directory (`.cache`, `Strategies`) under
//! `System.getProperty("user.home")` and `Bridge.java` overrides nothing, so an un-steered sidecar
//! puts the platform cache in `$HOME/JForex` — MEASURED on the production box, outside the project
//! folder entirely. `deploy/vike-tradehub.service` runs with `ProtectHome=yes`, which makes `/home`
//! INACCESSIBLE in that namespace rather than merely read-only, so the first LIVE Dukascopy mount
//! under the daemon could not create it; in a container `$HOME` may be `/` or unset and the
//! writable layer is gone on the next `docker run`, so the >60s first platform fetch this file's
//! [`READY_TIMEOUT`] is sized for would be paid on every start.
//!
//! [`jvm_args`] therefore prepends `-Duser.home=<project>/settings/state/jforex`, which that unit's
//! `ReadWritePaths=` already grants — the fix widens no sandbox and changes no unit. What it is NOT
//! is a per-sidecar cache: one process gets one home, so the one-sidecar-per-process refusal
//! (`crate::adapter`'s declaration, enforced by `vike-tradehub-core`) stands exactly as it did.
//!
//! # The three runtime facts are RESOLVED BY THE CALLER — this module decides nothing
//!
//! [`ExecutionClientDukascopy::spawn`] takes a [`DukascopyTools`], which the composition root
//! produces with `vike_dukascopy::resolve_dukascopy_tools` from its own environment sweep,
//! `vike_paths::state_path::project_bin_dir_from` and
//! `vike_paths::state_path::project_state_dir_from`. Nothing here reads the environment, walks for
//! a project, or knows where a jar lives.
//!
//! ⚠ **It used to, and that is worth keeping written down**: the deleted `bridge_jar` and
//! `java_program` built their fallbacks from `concat!(env!("CARGO_MANIFEST_DIR"), …)` — the tree
//! this crate was COMPILED in, not the project the binary is later run from. A binary built
//! elsewhere and deployed into a `<project>` folder found neither jar nor JVM and degraded to
//! paper, with nothing in the log but "bridge jar not found" — the same defect class the
//! credential-store ratchet spent a release retiring for settings. The escape hatches were
//! asymmetric too: `JFOREX_BRIDGE_JAR` and `JAVA_HOME` named the two artifacts outright, but the
//! `vendor/tools/` sweep beneath them had **no override at all**, so a deployment had to set both
//! explicitly or get nothing. Both variables were `Layer::Library` rows in
//! `crates/vike-config/src/settings.rs`; they are `Layer::Injected` rows now, read from the map the
//! caller supplies, and `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` ratchet shrank
//! by two. The ladders themselves live with the parser, on
//! `crates/bridges/dukascopy/src/config.rs`'s `resolve_dukascopy_tools`.

pub(crate) mod netting;

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, LazyLock, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use vike_bridge_core::link_gap::{GAP_SUMMARY_EVERY, GapWindow};
use vike_bridge_core::transport::ErrorKind;
use vike_bridge_core::venue_link::{VenueLink, VenueLinkDown, VenueLinkState, venue_link};
use vike_execution::{EventSender, ExecutionClient};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCancelRejected, OrderCanceled, OrderModifyRejected,
    OrderPartiallyFilled, OrderRejected, OrderSubmitted, PositionSide,
};
use vike_model::{FillReport, OrderRequest};
use vike_utility::mutex::lock;
use vike_utility::thread::join_within;
use vike_venues::reconnect::ReconnectBackoff;
use vike_venues::venue_timings::{self, VenueTimingsReconnect};

use super::config::{DukascopyConfig, DukascopyTools};
use crate::account::ready_balance::wallet_frame;
use crate::execution::netting::{PositionsShadow, PositionsShadowReanchor};
use crate::feed::{LinePort, QuoteBus};
use crate::reconcile::query::{ReconcileAnswer, ReconcileLink};
use crate::reconcile::{ReconcileClientDukascopy, ReconcileSource};
use crate::transport::{Envelope, SidecarCommand, StartFailure, encode_line, parse_envelope};

/// Shared tracing target for this module's diagnostics.
const TARGET: &str = "vike_dukascopy::execution";

/// The READY line's feature word for the reconcile QUERY (`Proto.java`'s `FEATURES`): the jar answers a `recon` command with
/// ONE `recon` envelope, what the venue lists now (`ReconSnapshot.java`). A jar that does not announce it cannot be
/// asked, so the start hands the node no reconcile client for it (`crate::adapter`'s `start_with`) and a question put to
/// it is an error in words (`crate::refusals::DukascopyReconcileRefusal::JarPredatesRecon`).
const FEATURE_RECONCILE: &str = "recon";

/// The READY line's feature word for the `modify` command (`Proto.java`'s `ready`). A jar that does not announce it
/// cannot amend.
const FEATURE_MODIFY: &str = "modify";

/// The READY line's feature word for ADOPTION (`Proto.java`'s `FEATURES`): at its start the sidecar takes up the orders
/// the venue already lists (a label names the order_client_id), sends the `position` lines and the `adopted` census, and
/// answers a stop sent again with the take-over of the price the order already carries. A client that reads it lifts
/// the `stranded` refusal for the adopted order client ids and keeps its resting protections across a sidecar's death to send them
/// again; a jar that does not announce it gets the behaviour this client had before (the refusal, the stop ended).
const FEATURE_ADOPT: &str = "adopt";

/// The READY line's feature word for the native TAKE-PROFIT: a reduce-only `limit` is the take-profit price of the
/// open orders (`StrategyBridge.java`'s `handleStop` with `Protection.TAKE_PROFIT`). A jar that does not announce it
/// would place such an order as a plain pending order that OPENS a position when hit, so it is refused by name.
const FEATURE_TAKE_PROFIT: &str = "take-profit";

/// The READY line's feature word for the venue's own conditional ENTRY orders: a `stop_limit` and a `take_profit`
/// (`TriggerEntry.java`: a stop or a limit command carrying a slippage, amended by `setOpenPrice(trigger, slippage)`,
/// named by kind at the recon query). A jar that does not announce it refuses both as "unsupported order_type", so
/// they are refused by name here, terminally, before anything is sent.
const FEATURE_TRIGGER_ENTRY: &str = "trigger-entry";

/// The child's stdin, shared by the exec client (its submits, cancels, amends and the shutdown) and the price lane
/// (`crate::feed`' subscribe commands). One mutex, one whole line per write, so two writers never interleave a line.
/// `None` is the dead-bridge marker: a failed write or the shutdown leaves it, and every later write is refused.
type Stdin = Mutex<Option<ChildStdin>>;

impl LinePort for Stdin {
    fn send(&self, line: &str) -> bool {
        let mut slot = self.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(stdin) = slot.as_mut() else { return false };
        let ok = writeln!(stdin, "{line}").and_then(|_| stdin.flush()).is_ok();
        if !ok {
            *slot = None;
        }
        ok
    }
}

/// JForex JNLP login is slow — first login downloads platform config (observed > 60s);
/// the sidecar polls the session for up to 240s, so bound the handshake just above that. Since the
/// stall cure it is the TOTAL cap over every start of one `spawn` ([`ATTEMPT_READY_TIMEOUT`] bounds
/// each), and with 3 x 25 s the cap is never the binding one.
///
/// The VALUE is the exec lane's `handshake_timeout` in
/// `vike_venues::venue_timings::DUKASCOPY`, which carries the reason; the NAME stays because
/// what this bounds is the sidecar becoming READY (the wait for its `Ready`/`Fatal` envelope
/// before the child is killed), and the Java sidecar's and the start's docs cite it by that name.
const READY_TIMEOUT: Duration = venue_timings::DUKASCOPY.exec().handshake_timeout();
/// How long ONE sidecar start may stay silent before it is killed and started again.
///
/// MEASURED (2026-10-17, the v0.1.71 deploy): 29 logins since 10-16 reached `bridge ready` 2-3 s
/// after the SDK's `Authorize task in queue` line; the one that hung there never completed, held the
/// tradehub's boot for 70+ s, and a fresh sidecar 8 s later was ready in 5 s. 25 s is 8x the normal
/// login. A compiled bound like `SHUTDOWN_GRACE` (decisions 0086, 0111), not a settings row.
///
/// ⚠ A FRESH platform cache's first login is documented to take over 60 s
/// (`crates/bridges/dukascopy/CLAUDE.md`'s "Login-failure triage"); on such a box every attempt is
/// cut at 25 s and the start gives up after ~75 s where it used to wait for the sidecar's own 240 s.
const ATTEMPT_READY_TIMEOUT: Duration = Duration::from_secs(25);
/// Sidecar starts per `spawn`: the first plus two respawns after a silent one.
const MAX_SPAWN_ATTEMPTS: u32 = 3;
/// Grace between `shutdown` and kill.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// The supervisor's wait before it starts another sidecar after one ended, and how that wait grows: the exec lane's
/// `reconnect` of the `DUKASCOPY` row (5 s doubling to 60 s, decision 0160: the row carries the reason, never retune
/// this constant). A `const` read, so a row that declares none is a COMPILE error here.
const RESTART: VenueTimingsReconnect = venue_timings::DUKASCOPY.exec().reconnect();
/// The supervisor's liveness bounds of a living sidecar: the exec lane's `keepalive_every` (the probe) and
/// `idle_threshold` (the limit) of the `DUKASCOPY` row, which carries the FX-open measurement. A `const` read, so a row
/// that declares either no more is a COMPILE error here.
const LIVENESS: Liveness = Liveness::of(venue_timings::DUKASCOPY.exec());
/// How many times per probe interval the parked supervisor looks at the living sidecar's silence (every 10 s at the
/// row's 60 s): the probe and the kill are each at most this late. A tick, no venue fact.
const LIVENESS_LOOKS_PER_PROBE: u32 = 6;
/// The death's words when the thread reading a sidecar's output panicked (a bug): the sidecar is ended as a dead one is.
const READER_PANICKED: &str = "the thread reading its output panicked (a bug; the log has its message), so the \
     sidecar was stopped and is started again like a dead one";
/// The account's link while the client's supervisor thread is dead: a named sub-path of its cell that gave up.
const SUPERVISOR_PATH: &str = "sidecar supervisor";
const SUPERVISOR_PANICKED: &str =
    "the JForex sidecar's supervisor thread panicked (a bug; the log has its message)";
const SUPERVISOR_EFFECT: &str = "the sidecar that runs now still serves orders, but one that dies or wedges is NOT \
                                 started again until the adapter restarts (set the account's `account.active` off \
                                 and on)";
/// The opening words of every refusal while no sidecar answers: a submit's `OrderRejected`, a cancel's
/// `OrderCancelRejected`, an amend's `OrderModifyRejected`. It was the whole reason before the supervisor existed.
const BRIDGE_UNAVAILABLE: &str = "bridge unavailable";
/// The `OrderRejected` reason of an order the sidecar had answered and that was still in flight when the sidecar died (a
/// resting limit), or that a clean stop ends. An order it never answered is not rejected at either ([`settle_unanswered`]).
/// It opens with the words it always had.
const BRIDGE_DIED: &str = "bridge died: the JForex sidecar exited while this order was in flight, so the venue's \
     state of it is unknown (look at the JForex platform)";
/// The `OrderCanceled` reason of a native stop-loss that was RESTING (the sidecar had accepted it) when its sidecar
/// died. Not a `Rejected`: the core folds a rejection only from `Initialized`/`Submitted` and DROPS it on an
/// `Accepted` order, which then stayed `Accepted` for ever, a stop the node believed in and nobody tracked. It opens
/// with the words of [`BRIDGE_DIED`]. ⚠ It only changes what the NODE believes: nothing here takes the stop-loss off
/// at the venue (a restart never cancels a resting order), and the words say so.
const STOP_BRIDGE_DIED: &str = "bridge died: the JForex sidecar exited while this stop-loss was resting, so the \
     node no longer tracks it and will not hear it fire. The venue may STILL hold the stop-loss (nothing took it \
     off): check the JForex platform, and count the position as unprotected by the node until a stop at the same \
     price is sent again to take it over";
/// The `OrderCanceled` reason of a native TAKE-PROFIT that was resting when a sidecar that does not adopt died: the stop-loss's
/// twin ([`STOP_BRIDGE_DIED`]), for the same reason and with the same limit (the node's belief only, nothing sent).
const TAKE_PROFIT_BRIDGE_DIED: &str = "bridge died: the JForex sidecar exited while this take-profit was resting, so the \
     node no longer tracks it and will not hear it fire. The venue may STILL hold the take-profit (nothing took it \
     off): check the JForex platform, and count the position as unprotected by the node until a take-profit at the \
     same price is sent again to take it over";
/// The `OrderCanceled` reason of a protection whose cancel was in flight when its sidecar died and whose price the venue,
/// read by the next sidecar, no longer holds on any open order ([`settle_cancels_in_flight`]).
const CANCEL_IN_FLIGHT_GONE: &str = "canceled: the JForex sidecar exited while this cancel was in flight; the next \
     sidecar read the venue and no open order carries this price any more (the cancel landed, or the price fired or was \
     taken off while no sidecar listened: look at the position in the JForex platform). Nothing was set again";
/// The `OrderCanceled` reason of a protection whose cancel was in flight when its sidecar died, when the next sidecar's
/// jar cannot list the venue's open orders (its `ready` does not announce `recon`): it is not sent again blind.
const CANCEL_IN_FLIGHT_UNREAD: &str = "canceled in the node's books only: the JForex sidecar exited while this cancel \
     was in flight, and the installed jar cannot list the venue's open orders (its `ready` does not announce `recon`), \
     so nothing was set again. The venue may STILL hold the price: check the JForex platform and take it off there";
/// The `OrderCanceled` reason of a REDUCE-ONLY MARKET close (a flatten, a margin call, a market exit, a liquidation) the
/// sidecar had taken up (its synthetic net-accept) when it died. Not a `Rejected`, for the same reason as
/// [`STOP_BRIDGE_DIED`]: the core would drop a rejection of an `Accepted` order and the close would stay `Accepted` for
/// ever, counted as cover by `closes_in_flight` while the venue's state is unknown. It opens with the words of
/// [`BRIDGE_DIED`]. ⚠ It changes what the NODE believes and sends nothing: the venue may have executed the close, and a
/// close that went through leaves the node's position open (the fill died with the sidecar), which only a look at the
/// position settles.
const CLOSE_BRIDGE_DIED: &str = "bridge died: the JForex sidecar exited while this reduce-only close was under way, so \
     the venue may have executed it, wholly or in part, and the node was never told (a close that went through leaves \
     the node's position open while the venue's is smaller or gone). Look at the position in the JForex platform. \
     This order is ended here so that it does not stay open for ever and keep the node counting a close in flight; a \
     new close is served, or refused by the venue in words when nothing is left to close";
/// The `OrderCanceled` reason of a plain MARKET order (an entry) the sidecar had acknowledged (its `OrderAccepted` for
/// the venue's `ORDER_SUBMIT_OK`, or the synthetic net-accept) when it died, when no venue read settles it: the dying or
/// the next jar does not adopt or cannot be asked (`recon`), or a part of its fill was already reported (the read cannot
/// say which part of the venue's row is new). An adopting jar's next sidecar reads the venue for any other one
/// ([`settle_unanswered`]). Not a `Rejected`, for the same reason as [`STOP_BRIDGE_DIED`]: the core would drop it and
/// the order would stay `Accepted` for ever. It opens with the words of [`BRIDGE_DIED`]. ⚠ It changes what the NODE
/// believes, sends nothing and books nothing (a cancel carries no fill): a fill that went through is a position at the
/// venue the node does not hold, which only a look at the venue settles.
const ENTRY_BRIDGE_DIED: &str = "bridge died: the JForex sidecar exited after the venue had accepted this market order \
     and before its fill reached the node, so the venue may have FILLED it, wholly or in part, and the node was never \
     told. Nothing was booked for it here, and no read of the venue could settle it: a fill that went through is a \
     position at the venue that the node does not hold. Look at the positions in the JForex platform. This order is \
     ended here so that it does not stay open for ever";
/// The `OrderCanceled` reason of a market entry the sidecar had acknowledged when it died, when the next sidecar's read
/// of the venue ([`settle_unanswered`]) lists no order under its label: the venue holds no position it opened.
const ENTRY_GONE: &str = "canceled: the JForex sidecar exited after the venue had accepted this market order and \
     before its fill reached the node; the next sidecar read the venue, which lists no order under this order's label, \
     so nothing was booked. The venue ended it unfilled, or it netted against the orders facing it (a close the read \
     cannot see): look at the position in the JForex platform";
/// How often a sidecar start that is waiting for `ready` looks up to see whether its client was stopped meanwhile,
/// so that a detach never waits for a login.
const HANDSHAKE_POLL: Duration = Duration::from_millis(100);
/// The most orders one client remembers having stranded across sidecar deaths (see `Shared::stranded`). Past it the
/// extra ones are not remembered, which only costs the named refusal of their cancel (Java then skips it in silence).
/// shortcut: a flat cap, a client lives hours between deploys; upgrade if a venue ever rests thousands of orders.
const MAX_STRANDED: usize = 4096;

/// The JVM system-property flag that relocates the sidecar's home directory: `-Duser.home=`.
///
/// ⚠ **A JVM OPTION, so it must precede `-jar`** — see [`jvm_args`], which is the only place it is
/// ever spelled.
const USER_HOME_FLAG: &str = "-Duser.home=";

/// The sidecar JVM's explicit MAXIMUM HEAP: `-Xmx512m` (512 MiB). Without it the JVM's default is a
/// quarter of the memory it believes it has, so the heap was whatever the host's cgroup happened to
/// say, not a number this crate chose.
///
/// ⚠ **The value is the default the one daemon that can launch the sidecar today already got, so
/// the flag changes nothing there.** That launcher is the trading daemon's execution path (the
/// gateway daemon starts no JVM until the trade lane moves into it, and production wires no
/// Dukascopy engine yet: `crate::adapter`'s module doc). The JRE is Temurin 17 (container-aware:
/// `MaxRAMPercentage` 25), and the trading daemon's unit has `MemoryMax=2G`
/// (`deploy/vike-tradehub.service`), so its default heap is 2 GiB / 4 = 512 MiB exactly. Under
/// `deploy/vike-gatewayhub.service` (`MemoryMax=8G`) the default would be 8 GiB / 4 = 2 GiB, and on
/// a host with no cgroup limit a quarter of the machine; this pins both to the 512 MiB the sidecar
/// is sized for. A reading of the JDK's ergonomics, not a measurement: a real JVM under a unit's
/// cgroup was not run (`crates/bridges/dukascopy/CLAUDE.md`'s "The JVM's heap is capped").
///
/// A COMPILED bound, not a settings row: it is a safety bound sized against a unit's `MemoryMax`
/// (the unit comment carries the sum), the way `SHUTDOWN_GRACE` and the venue-timing table
/// (decision 0160) are compiled; no process environment may carry it (decision 0095).
///
/// ⚠ **A JVM OPTION, so it must precede `-jar`** — see [`jvm_args`], which is the only place it is
/// ever spelled.
const MAX_HEAP_FLAG: &str = "-Xmx512m";

/// Errors from the Dukascopy venue layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DukascopyError {
    /// No jar / no java / login failed / handshake timed out — the account does not run. The kind
    /// (`vike_bridge_core::transport::ErrorKind`) says whether a later start can pass: [`ErrorKind::Timeout`] when
    /// no sidecar said `ready` in time, the sidecar's `fatal` read by `fatal_kind`, and [`ErrorKind::Unknown`]
    /// (it parks) for every failure that is not the venue's (no jar, no `java`, a sidecar dead without a word).
    Unavailable(ErrorKind),
}

impl std::fmt::Display for DukascopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DukascopyError::Unavailable(_) => {
                write!(f, "Dukascopy JForex bridge unavailable (jar/java missing or login failed)")
            }
        }
    }
}

impl std::error::Error for DukascopyError {}

/// Why one sidecar start produced no sidecar (see `ExecutionClientDukascopy::spawn_with_attempts`).
enum AttemptFailed {
    /// No `ready` and no `fatal` within the wait: the one failure worth a fresh sidecar at once.
    NoReady,
    /// A verdict (spawn error, `fatal`, silent death), in the sidecar's own words and of its kind: starting again at
    /// once repeats it.
    Refused(String, ErrorKind),
}

/// **What kind of failure a `fatal` the sidecar sent before `ready` is**, decided on the line's `failure` word
/// ([`StartFailure`]; `crates/bridges/dukascopy/jforex-bridge/src/main/java/vike/jforex/Bridge.java`'s
/// `reportStartupFailure` names it), never on its reason's words. The start folds it
/// (`vike_bridge_core::venue_adapter::AdapterOutcome::failed`): only a failure to REACH Dukascopy's servers is started
/// again; a refused login, a bad configuration or a JVM fault parks.
/// * [`StartFailure::DescriptorUnreachable`], the JNLP descriptor that could not be fetched (Dukascopy's own server,
///   the login never reached): [`ErrorKind::ServerError`];
/// * [`StartFailure::LoginTimedOut`] (the SDK neither connected nor refused within the sidecar's session poll):
///   [`ErrorKind::Timeout`];
/// * [`StartFailure::LoginUnreachable`] (the login threw, and it or a cause of it is the JDK's failure of a socket that
///   reached no server: the jar walks the cause chain, `Bridge.java`'s `reachedNoServer`): [`ErrorKind::Network`];
/// * [`StartFailure::LoginRefused`] (the login threw, and nothing in the chain reached no server: a refused login, a
///   corrupted platform cache): [`ErrorKind::Unknown`], which parks;
/// * anything else (missing settings, a JVM error, a descriptor miss followed by a different failure, none of which
///   names a failure word): [`ErrorKind::Unknown`], which parks.
///
/// Neither of the two login words is read by its class or its words (DK-5, decision 0301). shortcut: a jar older than
/// them names every login that threw [`StartFailure::LoginFailed`], so for that word alone the throwable's class
/// (`NETWORK_CLASSES`), the JDK's own words for a socket that reached no server and a whole `message` that is a bare
/// lowercase host (`java.net.UnknownHostException`'s) are still read, to the same verdicts; and a jar older than the
/// `failure` word (or a word this reader does not know) is read by its reason's words. Drop those arms once no box
/// runs such a jar.
pub(crate) fn fatal_kind(
    reason: &str,
    class: Option<&str>,
    failure: Option<&str>,
    message: Option<&str>,
) -> ErrorKind {
    const NETWORK_CLASSES: &[&str] = &[
        "java.net.UnknownHostException",
        "java.net.ConnectException",
        "java.net.NoRouteToHostException",
        "java.net.SocketTimeoutException",
        "java.net.SocketException",
    ];
    const NETWORK_WORDS: &[&str] = &[
        "connection refused",
        "connection reset",
        "timed out",
        "network is unreachable",
        "no route to host",
        "name or service not known",
        "temporary failure in name resolution",
        "remote host terminated the handshake",
    ];
    // The JDK's words: what a login failure that reached no server says, beside its class.
    let reached_no_server = |why: &str| {
        // A bare host (`platform.dukascopy.com`) is all `java.net.UnknownHostException` says. Lowercase only: a
        // dotted name with a capital is a Java class (a corrupted cache's `ClassNotFoundException`), not a host.
        let host = |b: u8| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-');
        class.is_some_and(|class| NETWORK_CLASSES.contains(&class))
            || NETWORK_WORDS.iter().any(|word| why.to_ascii_lowercase().contains(word))
            || (why.contains('.') && why.bytes().all(host))
    };
    let login_failed = |reached_no_server: bool| {
        if reached_no_server { ErrorKind::Network } else { ErrorKind::Unknown }
    };
    match failure.and_then(StartFailure::from_word) {
        Some(StartFailure::DescriptorUnreachable) => ErrorKind::ServerError,
        Some(StartFailure::LoginTimedOut) => ErrorKind::Timeout,
        Some(StartFailure::LoginUnreachable) => ErrorKind::Network,
        Some(StartFailure::LoginRefused) => ErrorKind::Unknown,
        // A jar older than the two words above: its class and the JDK's words (the shortcut above).
        Some(StartFailure::LoginFailed) => {
            login_failed(reached_no_server(message.unwrap_or_default()))
        }
        // A jar older than the word: its reason, in the words it has always used (the second shortcut above).
        None if reason.starts_with("JForex platform descriptor (JNLP) could not be fetched") => {
            ErrorKind::ServerError
        }
        None if reason == "login timed out" => ErrorKind::Timeout,
        None => login_failed(reason.strip_prefix("login failed: ").is_some_and(reached_no_server)),
    }
}

/// **Where the client's JForex sidecar is**, as `submit`, `cancel` and `modify` read it and as a test or a status reader
/// can ask ([`ExecutionClientDukascopy::sidecar_state`]), in the shared `VenueLinkState`: `Up` while a sidecar that said
/// `ready` has not ended (`Unknown` before the first one has: owner ruling U-6, nothing known yet is never up);
/// `Down` while none runs and the client's supervisor is starting another, built here. `why`: what the sidecar said when
/// it ended (its `fatal` reason), or the last failed restart's reason. `failed_restarts`: restarts that were refused or
/// said nothing in time, plus each restarted sidecar that died again before it had lived `RESTART.max` (the count only
/// starts over after a sidecar that lived that long), and at least one: a dead sidecar is a failure (`Error`, said at
/// once), never a routine reopen (`VenueLinkDown::failed`). `retry_in`: the wait before the next restart. No effect words.
fn sidecar_down(why: String, failed_restarts: u32, retry_in: Duration) -> VenueLinkState {
    VenueLinkState::Down(VenueLinkDown::failed(why, "", failed_restarts, retry_in))
}

/// The words a refusal carries while no sidecar runs; `None` while one does.
fn down_words(state: &VenueLinkState) -> Option<String> {
    match state {
        VenueLinkState::Up | VenueLinkState::Unknown => None,
        VenueLinkState::Down(VenueLinkDown { why, failed_restarts, retry_in, .. }) => {
            Some(format!(
                "the JForex sidecar is down ({why}); the node restarts it by itself \
             ({failed_restarts} failure(s) so far, next try in {retry_in:?})"
            ))
        }
    }
}

/// One sidecar process the client owns: the child and the thread reading its stdout.
struct SidecarProcess {
    child: Child,
    reader: std::thread::JoinHandle<()>,
}

impl SidecarProcess {
    /// Kill the child, reap it and join its reader. Idempotent on a child that already exited.
    fn kill_and_reap(self) {
        let SidecarProcess { mut child, reader } = self;
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
    }
}

/// A sidecar that reached `ready`, as `start_sidecar` hands it to its caller.
struct Started {
    session: SidecarProcess,
    /// `IAccount.getAccountId()` from the `ready` line.
    account: String,
    /// Whether the `ready` line announced the `modify` feature.
    amend_supported: bool,
}

/// An order in flight (until its terminal event), and how far the sidecar took it. The core takes a rejection only
/// from `Initialized`/`Submitted` (`crates/vike-model/src/orders/record.rs`'s `transition`) and DROPS it on an order
/// past that edge, so once the sidecar has acknowledged an order (`OrderAccepted`, or a first partial fill) its end
/// must be a CANCEL: at a sidecar's death ([`Underway::cancel_words_at_death`]) and when the venue refuses it after
/// all ([`settle`]).
#[derive(Debug, Clone, Copy)]
struct Underway {
    kind: UnderwayKind,
    /// Whether the sidecar has said `OrderAccepted` or `OrderPartiallyFilled` for it.
    acknowledged: bool,
    /// The quantity asked for, in units.
    qty: f64,
    /// The units the sidecar's `OrderPartiallyFilled` wraps have reported so far.
    filled: f64,
}

/// What an order is, for its end at a sidecar's death ([`Underway`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnderwayKind {
    /// A native stop-loss: it rests at the venue and survives the sidecar ([`STOP_BRIDGE_DIED`]).
    StopLoss,
    /// A native take-profit (a reduce-only limit): the stop-loss's twin ([`TAKE_PROFIT_BRIDGE_DIED`]).
    TakeProfit,
    /// A reduce-only market order: a close whose execution is unknown ([`CLOSE_BRIDGE_DIED`]).
    Close,
    /// A plain market order: an entry the venue may have filled ([`ENTRY_BRIDGE_DIED`]).
    Entry,
    /// Any other order (a limit): it keeps the rejection at a death, as it always did, its venue state unknown by
    /// design (`crates/bridges/dukascopy/CLAUDE.md`'s "The sidecar is restarted by itself").
    Resting,
}

impl Underway {
    fn of(request: &OrderRequest) -> Self {
        let kind = match request.order_type {
            vike_model::OrderType::Stop => UnderwayKind::StopLoss,
            vike_model::OrderType::Limit if request.reduce_only => UnderwayKind::TakeProfit,
            vike_model::OrderType::Market if request.reduce_only => UnderwayKind::Close,
            vike_model::OrderType::Market => UnderwayKind::Entry,
            _ => UnderwayKind::Resting,
        };
        Self { kind, acknowledged: false, qty: request.qty, filled: 0.0 }
    }

    /// Whether this is a native protection (a stop-loss or a take-profit): the venue holds its price, so it outlives a
    /// sidecar, and a sidecar that adopts is sent it again.
    fn is_protection(&self) -> bool {
        matches!(self.kind, UnderwayKind::StopLoss | UnderwayKind::TakeProfit)
    }

    /// The words of the `OrderCanceled` this order ends with at a sidecar's death, or `None` for the `OrderRejected` of
    /// a resting limit (and for an order the sidecar never answered, which the drain does not end).
    fn cancel_words_at_death(&self) -> Option<&'static str> {
        if !self.acknowledged {
            return None;
        }
        match self.kind {
            UnderwayKind::StopLoss => Some(STOP_BRIDGE_DIED),
            UnderwayKind::TakeProfit => Some(TAKE_PROFIT_BRIDGE_DIED),
            UnderwayKind::Close => Some(CLOSE_BRIDGE_DIED),
            UnderwayKind::Entry => Some(ENTRY_BRIDGE_DIED),
            UnderwayKind::Resting => None,
        }
    }
}

/// The units a reduce-only close may end short of its quantity and still read as filled: the sidecar reports whole
/// units (`OrderMap.units`), so anything under half a unit is float dust.
const SHORT_CLOSE_DUST: f64 = 0.5;

/// What the reader hands the core for one sidecar event: the event, or what the core can fold in its place, and a
/// terminal to follow it. It keeps `underway` up to date first (an acknowledgement, a partial fill's units).
///
/// * **A rejection of an order the sidecar had acknowledged becomes `OrderCanceled`, with the venue's words.** The
///   jar answers a venue's `ORDER_FILL_REJECTED` after its `ORDER_SUBMIT_OK` with `OrderRejected` (fixed in the jar of
///   the release after v0.1.96, which sends the cancel itself), and the core DROPS a rejection of an `Accepted` order:
///   it stayed `Accepted` for ever. This holds the end for every jar, and for any other path that ends an accepted
///   order with a rejection.
/// * **A reduce-only close that ends filled SHORT of its quantity ends `OrderPartiallyFilled` then `OrderCanceled`.**
///   The jar closes what the venue holds facing it and drops the rest (`NettingPlan.compute` with `reduceOnly`), then
///   reports the last leg `OrderFilled`, which read as a full close in `order ls`. The fill is the same (the bare
///   `Fill` lane is untouched, so nothing is booked differently); only the order's end and its words change, the
///   legal end of a partly filled order (FXCM's `CloseTracker` ends a close whose legs were not all sent the same way).
fn settle(underway: &mut HashMap<String, Underway>, event: Event) -> (Event, Option<Event>) {
    match event {
        Event::OrderAccepted(ref e) => {
            if let Some(u) = underway.get_mut(&e.order_client_id) {
                u.acknowledged = true;
            }
            (event, None)
        }
        Event::OrderPartiallyFilled(ref e) => {
            if let Some(u) = underway.get_mut(&e.order_client_id) {
                u.acknowledged = true;
                u.filled += e.fill.last_qty;
            }
            (event, None)
        }
        Event::OrderRejected(e)
            if underway.get(&e.order_client_id).is_some_and(|u| u.acknowledged) =>
        {
            let canceled = OrderCanceled {
                reason: format!(
                    "dukascopy: the venue ended this order after accepting it: {}",
                    e.reason
                )
                .into(),
                order_client_id: e.order_client_id,
                timestamp: e.timestamp,
                post_only_would_cross: false,
                // The cancel IS that rejection: it shows the rejection's own id (the jar sends none today).
                refusal_id: e.refusal_id,
            };
            (Event::OrderCanceled(canceled), None)
        }
        Event::OrderFilled(w) => match underway.get(&w.order_client_id) {
            Some(u)
                if u.kind == UnderwayKind::Close
                    && u.qty - (u.filled + w.fill.last_qty) > SHORT_CLOSE_DUST =>
            {
                let filled = u.filled + w.fill.last_qty;
                let reason = format!(
                    "reduce-only: the venue closed {filled} of this order's {} units and the other {} were NOT executed: \
                     a reduce-only order closes only what the venue holds facing it and never opens a position. If the \
                     node's position is larger than the venue's, look at the position in the JForex platform",
                    u.qty,
                    u.qty - filled
                );
                let canceled = OrderCanceled {
                    order_client_id: w.order_client_id.clone(),
                    reason: reason.into(),
                    timestamp: w.timestamp,
                    post_only_would_cross: false,
                    refusal_id: None,
                };
                let partial = OrderPartiallyFilled {
                    order_client_id: w.order_client_id,
                    fill: w.fill,
                    timestamp: w.timestamp,
                };
                (Event::OrderPartiallyFilled(partial), Some(Event::OrderCanceled(canceled)))
            }
            _ => (Event::OrderFilled(w), None),
        },
        other => (other, None),
    }
}

/// What the client, the reader of each sidecar and the supervisor share. It OUTLIVES every sidecar: the sidecars come
/// and go under it, and the book-keeping that must not restart with a process lives here.
struct Shared {
    events: EventSender,
    /// The current sidecar's stdin, shared with the price lane (`crate::feed`' subscribe commands). `None` inside is
    /// the dead-bridge marker: no sidecar runs, a write failed, or the client shut down. Each sidecar's start puts its
    /// pipe here, so the price lane's one `LinePort` keeps working across a restart.
    stdin: Arc<Stdin>,
    /// Every order awaiting its terminal event, and how far the sidecar took it ([`Underway`]). Inserted by `submit`
    /// BEFORE the write, so the reader never sees an event it cannot place; taken out by the order's terminal event,
    /// by `submit`'s failed write or by the reader's EOF drain, whichever comes first, and only that one ends the
    /// order, so a dead bridge never strands an order non-terminal and never ends one twice. The drain ends an order
    /// the sidecar has ACKNOWLEDGED with `OrderCanceled` ([`Underway::cancel_words_at_death`]: a stop-loss, a
    /// reduce-only close, a market entry), because the core drops a `Rejected` on an order past `Submitted`, and a
    /// resting limit with a rejection; one still unanswered (at a death, a clean stop or a start that never said `ready`
    /// alike: its submit was written) and, when the jar adopts, a market entry with no fill reported yet at a death, is
    /// not ended, [`Self::unanswered`].
    underway: Mutex<HashMap<String, Underway>>,
    /// The order client ids that were in flight (accepted, resting or not yet answered) when a sidecar died: ended
    /// with `bridge died` (an acknowledged stop-loss, close or entry with an `OrderCanceled`, a resting limit with an
    /// `OrderRejected`), or left for the next sidecar's read of the venue ([`Self::unanswered`]).
    /// The sidecar that replaced it has no memory of them (its label and order maps are in-process,
    /// `StrategyBridge.java`), so a cancel or an amend of one is refused by name here.
    stranded: Mutex<HashSet<String>>,
    /// The reconcile QUERY's link to the sidecar (`crate::reconcile::query`): the open questions, whether the current jar can
    /// answer one, and the symbols that get a flat row. `ReconcileClientDukascopy`'s source (`ReconcileHandle`).
    reconcile: ReconcileLink,
    /// The netting-truth shadow of the core's blended fold. It lives as long as the CLIENT, not the sidecar: it mirrors
    /// the core `Account`, which a sidecar restart does not reset.
    shadow: Mutex<PositionsShadow>,
    /// **Where the sidecar is, as this client last said it** (its refusals' words read it, the wait exactly as said),
    /// `Unknown` until the first sidecar has said `ready` (owner ruling U-6: nothing known yet is never up). Read and
    /// write it through [`Self::state`] / [`Self::set_state`], which publish every word to [`Self::link`].
    said: Mutex<VenueLinkState>,
    /// The account's ONE cell (`vike_bridge_core::venue_link::venue_link` of the start's route key, claimed by this
    /// client, so an earlier client's word is history and its writes land nowhere), which the gatewayhub's status verb
    /// reads, its wait counted down as the cell does for every venue.
    link: VenueLink,
    /// The last `fatal` reason the reader heard, for the words of the death.
    last_fatal: Mutex<Option<String>>,
    /// The sidecar that runs now (`None` while none does).
    session: Mutex<Option<SidecarProcess>>,
    /// Whether the CURRENT sidecar's `ready` announced `modify`.
    amend_supported: AtomicBool,
    /// Whether the CURRENT sidecar's `ready` announced [`FEATURE_ADOPT`]. Written by that sidecar's reader before the
    /// start is told it succeeded; the dying sidecar's value is still in place while its reader drains, which is what
    /// decides whether its resting protections are kept for the next one.
    adopts: AtomicBool,
    /// Whether the CURRENT sidecar's `ready` announced [`FEATURE_TAKE_PROFIT`].
    take_profit_supported: AtomicBool,
    /// Whether the CURRENT sidecar's `ready` announced [`FEATURE_TRIGGER_ENTRY`].
    trigger_entry_supported: AtomicBool,
    /// How many sidecars of this client have said `ready`. A census that finds a venue position no fold explains means
    /// something different at the second start than at the first (a gatewayhub restart is a first start).
    starts: AtomicUsize,
    /// The native protections (stop-loss, take-profit) this client sent, by order client id, as the node holds them: kept from the
    /// submit to the order's terminal event, and updated by a `modify`. A sidecar that ADOPTS dies and its successor is
    /// sent each acknowledged one again (`hand_back_protections`): the price the order already carries at the venue is
    /// taken over, which re-teaches the new process whose stop it is. Not kept for a jar that does not adopt.
    protections: Mutex<HashMap<String, OrderRequest>>,
    /// The protections whose `cancel` was written to a sidecar and has had no answer yet (a terminal, or the cancel's
    /// refusal): one still here when that
    /// sidecar dies is NOT sent to the next one blind (its submit would set the price again if the cancel had landed),
    /// the venue is read first ([`settle_cancels_in_flight`]).
    cancelling: Mutex<HashSet<String>>,
    /// The orders a sidecar that DIED had never answered: their submit's outcome is unknown (they may rest at the venue,
    /// decision 0303), so they are NOT rejected. With them, the market entries it had acknowledged and reported no fill
    /// of: the venue may have FILLED one. Kept in [`Self::underway`] (and a protection in [`Self::protections`]) until
    /// the next sidecar has read the venue ([`settle_unanswered`]); only when the dying jar adopts, because the next one
    /// (the same jar) can then take them up.
    unanswered: Mutex<HashSet<String>>,
    /// Set by the client's detach or drop, or by a reader whose core is gone: no sidecar is to be started and a
    /// start in progress is to be killed.
    stopping: AtomicBool,
    /// The wait policy between restarts (`RESTART`; a test passes its own).
    restart: VenueTimingsReconnect,
    /// The current sidecar's lines in this minute (`vike_bridge_core::link_gap`): started over by each sidecar's reader, noted on every
    /// line it reads, and said by the parked supervisor once a minute.
    gaps: Mutex<GapWindow>,
    /// When the CURRENT sidecar last wrote a line, counted from its `ready` ([`Liveness`]): `None` before it, for a jar
    /// that does not announce [`FEATURE_RECONCILE`] (no probe and no limit) and once the supervisor killed it as wedged.
    heard: Mutex<Option<Instant>>,
    /// The cancels and amends whose outcome JForex left open, until each is settled ([`Unsettled`]).
    unsettled: Mutex<Unsettled>,
}

impl Shared {
    fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    fn state(&self) -> VenueLinkState {
        lock(&self.said).clone()
    }

    fn set_state(&self, state: VenueLinkState) {
        let mut said = lock(&self.said);
        self.link.set(state.clone(), None);
        *said = state;
    }

    /// The reader of a sidecar that had said `ready` reached EOF while the client still runs: say so, ONCE, and
    /// refuse commands by name from here on.
    fn went_down(&self) {
        let why = lock(&self.last_fatal)
            .take()
            .unwrap_or_else(|| "its output ended and the process exited".to_string());
        tracing::error!(
            target: TARGET,
            %why,
            restart_in = ?self.restart.initial,
            "the JForex sidecar is DOWN: every order, cancel and amend is refused by name until the node has \
             restarted it"
        );
        self.set_state(sidecar_down(why, 0, self.restart.initial));
    }
}

/// Everything one more sidecar start needs; the supervisor keeps it for the life of the client.
struct Launch {
    program: String,
    args: Vec<String>,
    /// The login the first sidecar used, kept so the next one can log in again. Never logged (`DukascopyConfig`'s
    /// `Debug` omits the password) and never put on argv: it reaches the child through the environment.
    config: DukascopyConfig,
    quotes: Arc<QuoteBus>,
    /// How long ONE start may stay silent before it is killed.
    attempt_timeout: Duration,
    /// When a living sidecar is probed, and when it is called wedged ([`LIVENESS`]; a test passes its own).
    liveness: Liveness,
}

/// **A living sidecar's liveness** (a jar that announces [`FEATURE_RECONCILE`] only, counted from its `ready`): it writes
/// no heartbeat, and the node's reconcile pass, whose answers are its only steady lines (three a pass), can be off
/// (`flags.reconcile_off`) or stretched (`config.reconcile_interval_ms`). So the supervisor asks the `recon` question
/// itself once the sidecar has written nothing for `probe_after` (`ReconcileLink::probe`, under an id no reconcile asker is
/// given), and a sidecar that wrote nothing for `idle_limit` is WEDGED: it is killed, and its reader's end starts the
/// restart a death starts. A JForex session that ends is not this: it ends the sidecar itself (`fatal`, then exit).
#[derive(Debug, Clone, Copy)]
struct Liveness {
    probe_after: Duration,
    idle_limit: Duration,
}

impl Liveness {
    /// The bounds `lane` declares, read through its accessors (each panics on a lane that declares none).
    const fn of(lane: venue_timings::VenueTimingsLane) -> Self {
        Self { probe_after: lane.keepalive_every(), idle_limit: lane.idle_threshold() }
    }
}

/// The refusal of a cancel or an amend (`verb`) of an order that was in flight when a sidecar died.
fn stranded_words(verb: &str) -> String {
    format!(
        "dukascopy: the JForex sidecar was restarted after this order was placed and the new one does not \
         track it, so the {verb} was NOT sent; the order may still rest at the venue: {verb} it in the \
         JForex platform"
    )
}

/// **The order as the sidecar is sent it**: the same request under its instrument's WIRE name, read off the kept row
/// (`crate::instrument::names::wire_symbol`); the engine and every event name it by its USER name (decision 0290).
fn to_the_wire(request: &OrderRequest) -> Result<OrderRequest, String> {
    let symbol = crate::instrument::names::wire_symbol(&request.symbol)?;
    Ok(OrderRequest { symbol, ..request.clone() })
}

/// **A fill the sidecar names by the instrument's wire name, renamed by its user name** (`crate::instrument::names`), so
/// the engine books it, the shadow folds it and the reconcile owes it a flat row under the name its order went out with.
/// Every other event names no instrument.
fn fill_by_user_name(event: &mut Event) {
    let fill = match event {
        Event::Fill(fill) => fill,
        Event::OrderFilled(filled) => &mut filled.fill,
        Event::OrderPartiallyFilled(partly) => &mut partly.fill,
        _ => return,
    };
    fill.symbol = crate::instrument::names::user_symbol_or_wire(fill.symbol.as_str()).into();
}

/// A sidecar has just said `ready`: hand it back the native protections (stop-losses, take-profits) the node still holds
/// resting, which the previous sidecar's death left with no owner. Returns `false` when the core is gone.
///
/// * **A sidecar that adopts** (`adopts`) is SENT each one again, as the ordinary `submit` it was. The venue kept the price
///   on the order, so the new process finds it there and TAKES IT OVER: nothing is changed at the venue, and the stop is
///   the new process's again (its cancel, its modify and its fire work, and a fire is the stop's own closing fill). The
///   order is tracked as acknowledged, so a refusal ("no open long order to protect": the price fired or was taken off
///   while no sidecar listened) ends it `OrderCanceled` with the venue's words ([`settle`]), and the answer's
///   `OrderAccepted` is not a second acceptance of an order the node already holds accepted (`replaying`).
/// * **A jar that does not adopt** cannot take it over: each ends `OrderCanceled` in words, exactly as it did at the death
///   before the protections were kept (the node's belief only: nothing is sent to the venue).
/// * **A protection whose cancel was in flight at the death** (`Shared::cancelling`) is NOT sent yet: if the cancel had
///   landed, the order is still open without the price and the submit would SET it again (`StopPlan` takes over only an
///   equal price). It goes to `awaiting` and the venue is read first through this sidecar (the `recon` question under
///   `crate::reconcile::query::PROBE_ID`, whose answer the reader hands to [`settle_cancels_in_flight`]). A jar that
///   cannot be asked ends it `OrderCanceled` in words ([`CANCEL_IN_FLIGHT_UNREAD`]): nothing is set again blind.
/// * **An order the dead sidecar never answered** (`Shared::unanswered`, a protection among them, and a market entry whose
///   fill it never reported) is not sent either: the same read settles it ([`settle_unanswered`]); a jar that cannot be
///   asked, or does not adopt, leaves it to the core's stuck-order ladder ([`leave_unanswered_to_the_ladder`]).
fn hand_back_protections(
    shared: &Shared,
    adopts: bool,
    pump: &EventSender,
    replaying: &mut HashSet<String>,
    awaiting: &mut Vec<(String, OrderRequest)>,
) -> bool {
    if !adopts && !leave_unanswered_to_the_ladder(shared, pump) {
        return false;
    }
    let held: Vec<(String, OrderRequest)> =
        lock(&shared.protections).iter().map(|(c, r)| (c.clone(), r.clone())).collect();
    for (order_client_id, request) in held {
        // An unanswered protection is read at the venue first (`settle_unanswered`): never sent blind, never acknowledged.
        if lock(&shared.unanswered).contains(&order_client_id) {
            continue;
        }
        let mut underway = Underway::of(&request);
        underway.acknowledged = true;
        if adopts {
            lock(&shared.underway).insert(order_client_id.clone(), underway);
            if lock(&shared.cancelling).contains(&order_client_id) {
                awaiting.push((order_client_id, request));
                continue;
            }
            replaying.insert(order_client_id.clone());
            tracing::info!(
                target: TARGET,
                %order_client_id,
                "sending a resting protection to the new sidecar: it takes over the price the order already carries"
            );
            let sent = match to_the_wire(&request) {
                Ok(sent) => sent,
                Err(why) => {
                    // Its kept row is gone since it was first sent: kept as it is, like a write that failed below.
                    tracing::warn!(target: TARGET, %order_client_id, %why, "the resting protection could not be named for the new sidecar");
                    continue;
                }
            };
            let line = encode_line(&SidecarCommand::Submit { order: Box::new(sent) });
            if !shared.stdin.send(&line) {
                // The sidecar is already gone again: its reader's drain keeps the protection for the next one.
                tracing::warn!(target: TARGET, %order_client_id, "the resting protection could not be sent to the new sidecar");
            }
        } else {
            lock(&shared.protections).remove(&order_client_id);
            lock(&shared.cancelling).remove(&order_client_id);
            let words = underway.cancel_words_at_death().unwrap_or(BRIDGE_DIED);
            let ended = Event::OrderCanceled(OrderCanceled {
                order_client_id,
                reason: words.into(),
                timestamp: vike_model::now_ms(),
                post_only_would_cross: false,
                refusal_id: None,
            });
            if pump.blocking_send(ended).is_err() {
                return false;
            }
        }
    }
    let unanswered = lock(&shared.unanswered).len();
    if awaiting.is_empty() && unanswered == 0 {
        return true;
    }
    if !shared.reconcile.supported() {
        for (order_client_id, _) in awaiting.drain(..) {
            if !end_cancel_in_flight(shared, pump, order_client_id, CANCEL_IN_FLIGHT_UNREAD) {
                return false;
            }
        }
        if !leave_unanswered_to_the_ladder(shared, pump) {
            return false;
        }
    } else if shared.reconcile.probe() {
        tracing::info!(
            target: TARGET,
            protections = awaiting.len(),
            unanswered,
            "a cancel was in flight at the sidecar's death, or an order it never answered: reading the venue's open \
             orders before the new sidecar is sent any of them"
        );
    } else {
        // The sidecar is already gone again: its drain keeps them for the next start, which asks again.
        tracing::warn!(target: TARGET, "the venue read for the protections whose cancel was in flight was not sent");
    }
    true
}

/// End a protection whose cancel was in flight at a sidecar's death in the node's books, in `words`. Nothing is sent to
/// the venue. Returns `false` when the core is gone.
fn end_cancel_in_flight(
    shared: &Shared,
    pump: &EventSender,
    order_client_id: String,
    words: &str,
) -> bool {
    lock(&shared.underway).remove(&order_client_id);
    lock(&shared.protections).remove(&order_client_id);
    lock(&shared.cancelling).remove(&order_client_id);
    tracing::warn!(target: TARGET, %order_client_id, %words, "a protection whose cancel was in flight ends");
    pump.blocking_send(Event::OrderCanceled(OrderCanceled {
        order_client_id,
        reason: words.into(),
        timestamp: vike_model::now_ms(),
        post_only_would_cross: false,
        refusal_id: None,
    }))
    .is_ok()
}

/// The orders a dead sidecar never answered, when no venue read can settle them (the next jar does not adopt, or cannot be
/// asked): each stays `Submitted` on the node for the core's stuck-order ladder (its confirm, grace and last resort) and
/// reconcile. Nothing is sent; nothing reports them any more, so they leave the in-flight books. A market entry the
/// sidecar had ACKNOWLEDGED is past that ladder (it sweeps orders before their accept only), so it ends `OrderCanceled`
/// in the words a death without a read gives it ([`ENTRY_BRIDGE_DIED`]). Returns `false` when the core is gone.
fn leave_unanswered_to_the_ladder(shared: &Shared, pump: &EventSender) -> bool {
    let ids: Vec<String> = lock(&shared.unanswered).drain().collect();
    ids.iter().all(|order_client_id| {
        leave_to_the_ladder(shared, pump, order_client_id, ENTRY_BRIDGE_DIED)
    })
}

/// One order a dead sidecar left with no end, out of the in-flight books ([`leave_unanswered_to_the_ladder`]): one it never
/// answered is left `Submitted` for the core's stuck-order ladder and reconcile; a market entry it had acknowledged ends
/// `OrderCanceled` in `entry_words`. Returns `false` when the core is gone.
fn leave_to_the_ladder(
    shared: &Shared,
    pump: &EventSender,
    order_client_id: &str,
    entry_words: &str,
) -> bool {
    let acknowledged = lock(&shared.underway)
        .remove(order_client_id)
        .is_some_and(|underway| underway.acknowledged);
    lock(&shared.protections).remove(order_client_id);
    lock(&shared.cancelling).remove(order_client_id);
    if acknowledged {
        tracing::warn!(target: TARGET, %order_client_id, words = entry_words, "a market entry whose fill the dead sidecar never reported ends");
        return pump
            .blocking_send(Event::OrderCanceled(OrderCanceled {
                order_client_id: order_client_id.to_string(),
                reason: entry_words.into(),
                timestamp: vike_model::now_ms(),
                post_only_would_cross: false,
                refusal_id: None,
            }))
            .is_ok();
    }
    tracing::warn!(
        target: TARGET,
        %order_client_id,
        "an order the dead sidecar never answered is not held at the venue, or the venue cannot be read: it stays \
         submitted for the node's stuck-order watchdog and reconcile"
    );
    true
}

/// **Book the fill the venue read found for an order a dead sidecar left with no end** ([`settle_unanswered`]): the
/// order's whole fill as the venue states it (units, price, commission), under the trade id the reconcile leg reports it
/// by (`<order id>:1`, [`crate::reconcile::fill_reports`], the id the dead sidecar would have minted for it), so the
/// leg finds it folded and nothing books it twice. One the sidecar never answered is accepted first (the core has no
/// `Submitted -> Filled` edge); one filled short of its quantity ends `OrderPartiallyFilled` then `OrderCanceled` in
/// words. The shadow folds the bare fill (the dual-publish contract), unless this start's `position` line adopted the
/// symbol's venue position, this fill included, as its baseline (`baselines`). Returns `false` when the core is gone.
fn book_read_fill(
    shared: &Shared,
    pump: &EventSender,
    order_client_id: &str,
    underway: Underway,
    report: FillReport,
    baselines: &HashSet<String>,
) -> bool {
    lock(&shared.underway).remove(order_client_id);
    lock(&shared.stranded).remove(order_client_id);
    let now = vike_model::now_ms();
    let fill = FillEvent {
        trade_id: report.trade_id,
        order_client_id: order_client_id.to_string(),
        venue: report.venue.as_str().into(),
        symbol: report.symbol.as_str().into(),
        side: report.side,
        last_qty: report.last_qty,
        last_price: report.last_price,
        commission: report.commission,
        commission_asset: report.commission_asset.as_str().into(),
        liquidity_side: report.liquidity_side,
        timestamp: report.timestamp,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    tracing::warn!(
        target: TARGET,
        %order_client_id,
        units = fill.last_qty,
        price = fill.last_price,
        "the venue filled an order whose fill the dead sidecar never reported: booked from the next sidecar's read"
    );
    if !baselines.contains(fill.symbol.as_str()) {
        lock(&shared.shadow).fold_fill(&fill);
    }
    shared.reconcile.know(&fill.symbol);
    let filled = fill.last_qty;
    let whole = underway.qty - filled <= SHORT_CLOSE_DUST;
    let accept = (!underway.acknowledged).then(|| {
        Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.to_string(),
            venue_order_id: Some(report.venue_order_id),
            timestamp: now,
        })
    });
    let short = (!whole).then(|| {
        Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.to_string(),
            reason: format!(
                "the JForex sidecar exited before it reported this order's fill; the next sidecar read the venue, which \
                 filled {filled} of its {} units under its label: the other {} were NOT executed under it (a market \
                 order may have closed orders facing it, a close the read cannot see): look at the position in the \
                 JForex platform",
                underway.qty,
                underway.qty - filled
            )
            .into(),
            timestamp: now,
            post_only_would_cross: false,
            refusal_id: None,
        })
    });
    let booked =
        vike_bridge_core::event_map::terminal_events(order_client_id.to_string(), fill, now, whole);
    accept.into_iter().chain(booked).chain(short).all(|event| pump.blocking_send(event).is_ok())
}

/// **The venue read answers for the orders a dead sidecar never answered** (decision 0303: an unknown outcome is
/// resolved by a read keyed by the order client id, never rejected and never sent again blind), and for the market
/// entries it had acknowledged and reported no fill of. `answer` is `None` when the read failed: nothing is sent and
/// nothing ends, and the next answer under the probe's id or the next start reads again. Returns `false` when the core
/// is gone.
///
/// * **Already ended**: the new sidecar adopted the order and reported its end before this answer (both are written on
///   its strategy thread, in that order), so the reader has ended it: nothing is left to settle.
/// * **Held** (the reconcile report's own rule, [`crate::reconcile::order_reports`]: a resting order of ours under its
///   order client id, or a protection's price on an open order facing it): the submit reached the venue and the new
///   sidecar took the order up. A plain order is ACCEPTED here, with the venue's order id (the adopting sidecar
///   announces nothing for what it took up), unless the dead sidecar had already acknowledged it (an entry still being
///   placed: the new sidecar reports its fill); a protection is sent to the new sidecar, which takes over the price it
///   holds and answers with its own acceptance.
/// * **Filled**: the venue lists a FILLED order under its label (an entry that opened a position, a limit that filled
///   while no sidecar listened): its fill is BOOKED once ([`book_read_fill`]) and the order ends filled.
/// * **Not listed**: one never answered stays `Submitted` for the core's stuck-order ladder and reconcile, as when nothing
///   can read the venue ([`leave_unanswered_to_the_ladder`]): the read lists no closed order, so a market order that
///   netted against the orders facing it and an order the venue refused look alike, and neither is ended here. An
///   acknowledged entry, past that ladder, ends `OrderCanceled` ([`ENTRY_GONE`]).
///
/// `baselines`: the symbols whose `position` line at this sidecar's start the shadow adopted as its baseline.
fn settle_unanswered(
    shared: &Shared,
    answer: Option<&ReconcileAnswer>,
    pump: &EventSender,
    baselines: &HashSet<String>,
) -> bool {
    let Some(answer) = answer else { return true };
    let ids: Vec<String> = lock(&shared.unanswered).drain().collect();
    for order_client_id in ids {
        let Some(underway) = lock(&shared.underway).get(&order_client_id).copied() else {
            continue;
        };
        let protection = lock(&shared.protections).get(&order_client_id).cloned();
        let one: Vec<(String, OrderRequest)> =
            protection.iter().map(|request| (order_client_id.clone(), request.clone())).collect();
        let report = crate::reconcile::order_reports(answer, &one)
            .into_iter()
            .find(|report| report.order_client_id.as_deref() == Some(order_client_id.as_str()));
        let Some(report) = report else {
            let filled = crate::reconcile::fill_reports(answer, 0)
                .into_iter()
                .find(|fill| fill.order_client_id.as_deref() == Some(order_client_id.as_str()));
            let ended = match filled {
                Some(fill) => {
                    book_read_fill(shared, pump, &order_client_id, underway, fill, baselines)
                }
                None => leave_to_the_ladder(shared, pump, &order_client_id, ENTRY_GONE),
            };
            if !ended {
                return false;
            }
            continue;
        };
        lock(&shared.stranded).remove(&order_client_id);
        tracing::info!(
            target: TARGET,
            %order_client_id,
            "the venue holds an order the dead sidecar left with no end: the new sidecar took it up"
        );
        match protection {
            Some(request) => {
                let Ok(sent) = to_the_wire(&request) else {
                    // Its kept row is gone since it was first sent: left to the ladder, like one the venue does not hold.
                    if !leave_to_the_ladder(shared, pump, &order_client_id, ENTRY_GONE) {
                        return false;
                    }
                    continue;
                };
                if !shared
                    .stdin
                    .send(&encode_line(&SidecarCommand::Submit { order: Box::new(sent) }))
                {
                    // The sidecar is already gone again: its drain keeps the protection unanswered for the next start.
                    tracing::warn!(target: TARGET, %order_client_id, "the take-over could not be sent to the new sidecar");
                }
            }
            None if underway.acknowledged => {}
            None => {
                if let Some(underway) = lock(&shared.underway).get_mut(&order_client_id) {
                    underway.acknowledged = true;
                }
                let accepted = Event::OrderAccepted(OrderAccepted {
                    order_client_id,
                    venue_order_id: Some(report.venue_order_id),
                    timestamp: vike_model::now_ms(),
                });
                if pump.blocking_send(accepted).is_err() {
                    return false;
                }
            }
        }
    }
    true
}

/// **The venue read [`hand_back_protections`] asked for answers**: settle every protection whose cancel was in flight at the
/// last sidecar's death. Returns `false` when the core is gone.
///
/// * **The venue still holds its price** (an open order facing it carries it: the reconcile report's own rule,
///   [`crate::reconcile::order_reports`]): the new sidecar is sent the protection, which TAKES OVER the price it holds
///   (nothing changes at the venue; the answer's `OrderAccepted` is swallowed through `replaying`), then the cancel again,
///   which takes it off. shortcut: the take-over is `StopPlan`'s plan again (facing orders oldest first), so a leg whose
///   price the dead sidecar's cancel had already taken off, or an older unprotected facing order, is set for about a
///   second before the cancel takes it off; exact per-order cancels need a jar command, upgrade if a multi-order stop is
///   ever used live.
/// * **The venue no longer holds it**: it ends `OrderCanceled` ([`CANCEL_IN_FLIGHT_GONE`]) and nothing is sent. The read
///   lists no closed order and no closing fill, so a price that FIRED while no sidecar listened ends the same way (its fill
///   is the phantom of `crates/bridges/dukascopy/CLAUDE.md`, the census's flat warn), never `OrderFilled`.
/// * **The read failed** (`read` is an `Err`): nothing is sent and nothing ends; they stay `awaiting`, and the next answer
///   under the probe's id (the supervisor's liveness probe) or the next sidecar start reads again.
fn settle_cancels_in_flight(
    shared: &Shared,
    read: &crate::reconcile::query::Reply,
    awaiting: &mut Vec<(String, OrderRequest)>,
    replaying: &mut HashSet<String>,
    pump: &EventSender,
) -> bool {
    let answer = match read {
        Ok(answer) => answer,
        Err(why) => {
            tracing::warn!(
                target: TARGET,
                %why,
                protections = awaiting.len(),
                "the venue could not be read for the protections whose cancel was in flight: nothing is sent, they \
                 stay as the node holds them and are read again"
            );
            return true;
        }
    };
    for (order_client_id, request) in std::mem::take(awaiting) {
        let one = [(order_client_id.clone(), request.clone())];
        let held = crate::reconcile::order_reports(answer, &one)
            .iter()
            .any(|report| report.order_client_id.as_deref() == Some(order_client_id.as_str()));
        if !held {
            if !end_cancel_in_flight(shared, pump, order_client_id, CANCEL_IN_FLIGHT_GONE) {
                return false;
            }
            continue;
        }
        let Ok(sent) = to_the_wire(&request) else {
            // Its kept row is gone since it was first sent: kept as it is, like `hand_back_protections` does.
            tracing::warn!(target: TARGET, %order_client_id, "the protection whose cancel was in flight could not be named");
            continue;
        };
        tracing::info!(
            target: TARGET,
            %order_client_id,
            "the venue still holds the protection whose cancel was in flight: the new sidecar takes its price over and is \
             sent the cancel again"
        );
        replaying.insert(order_client_id.clone());
        // Still in flight (a node cancel refused while the read was open cleared it): a death before its answer reads again.
        lock(&shared.cancelling).insert(order_client_id.clone());
        let take_over = encode_line(&SidecarCommand::Submit { order: Box::new(sent) });
        let cancel = encode_line(&SidecarCommand::Cancel { order_client_id });
        if !(shared.stdin.send(&take_over) && shared.stdin.send(&cancel)) {
            // The sidecar is already gone again: its drain keeps the protection, and the next start reads again.
            tracing::warn!(target: TARGET, "the take-over and cancel could not be sent to the new sidecar");
        }
    }
    true
}

/// The `OrderCanceled` reason of a cancel whose outcome JForex left open ([`answer_unsettled`]) when a venue read that
/// followed shows it took ([`settle_unsettled`]).
const CANCEL_UNSETTLED_TOOK: &str = "canceled: JForex answered the cancel neither way within the sidecar's bound, and \
     the venue read that followed shows it took (the order is gone, or its price is on none of the open orders it \
     protected)";

/// The terms an amend whose outcome JForex left open asked for ([`Envelope::Unsettled`]'s `new_qty`, `new_price` and
/// `new_trigger_price`, as the sidecar sent them to JForex), which a venue read compares ([`settle_unsettled`]).
#[derive(Debug, Clone, Copy, PartialEq)]
struct AmendAsked {
    new_qty: Option<f64>,
    new_price: Option<f64>,
    new_trigger_price: Option<f64>,
}

impl AmendAsked {
    /// Whether the venue's row of a RESTING order holds every term the amend asked for: `Some(true)` every one,
    /// `Some(false)` one is not there (yet), `None` the row cannot show one of them (the limit of a stop-limit or an
    /// if-touched order is a slippage the SDK states no getter for, `crate::reconcile::order_reports`) or none was
    /// asked. A price compares as the sidecar's own `approx` does (`StrategyBridge.java`), a size in whole units.
    fn read_back(&self, report: &vike_model::OrderStatusReport) -> Option<bool> {
        let same = |held: f64, want: f64| (held - want).abs() <= 1e-9 * want.abs().max(1.0);
        let limit =
            (report.order_type == vike_model::OrderType::Limit).then_some(report.price).flatten();
        let asked: Vec<Option<bool>> = [
            self.new_qty.map(|want| Some((report.qty - want).abs() < SHORT_CLOSE_DUST)),
            self.new_price.map(|want| limit.map(|held| same(held, want))),
            self.new_trigger_price.map(|want| report.trigger_price.map(|held| same(held, want))),
        ]
        .into_iter()
        .flatten()
        .collect();
        if asked.is_empty() {
            return None;
        }
        asked.into_iter().try_fold(true, |every, term| term.map(|there| every && there))
    }
}

/// **The cancels and amends whose outcome JForex left open** ([`Envelope::Unsettled`], decision 0303), by order client
/// id: each until the sidecar's next answer to that command (JForex's late answer, or the answer to a newer one), the
/// order's own end, or a venue read settles it ([`settle_unsettled`]).
#[derive(Debug, Default)]
struct Unsettled {
    cancels: HashSet<String>,
    amends: HashMap<String, AmendAsked>,
}

impl Unsettled {
    fn is_empty(&self) -> bool {
        self.cancels.is_empty() && self.amends.is_empty()
    }

    /// The sidecar's next answer to a command settles what was open of that command.
    fn answered_by(&mut self, event: &Event) {
        match event {
            Event::OrderCancelRejected(refused) => {
                self.cancels.remove(&refused.order_client_id);
            }
            Event::OrderModified(modified) => {
                self.amends.remove(&modified.order_client_id);
            }
            Event::OrderModifyRejected(refused) => {
                self.amends.remove(&refused.order_client_id);
            }
            _ => {}
        }
    }

    /// The order ended: nothing of it is open any more.
    fn forget(&mut self, order_client_id: &str) {
        self.cancels.remove(order_client_id);
        self.amends.remove(order_client_id);
    }
}

/// **A cancel or an amend whose outcome JForex left open** ([`Envelope::Unsettled`]) is answered as decision 0303 answers
/// an unknown outcome, never as a refusal: the shared `vike_bridge_core::rest::cancel_rejected` / `modify_rejected` of an
/// error coded `E_TIMEOUT_AMBIGUOUS` (non-terminal, `order.outcome_unknown`, in words that say the venue may have done
/// it). It stays open in `Shared::unsettled`, a protection's cancel in `Shared::cancelling` too (a death before it
/// settles reads the venue before the protection is sent again, [`settle_cancels_in_flight`]), and the venue is read for
/// it at once (the `recon` question under `crate::reconcile::query::PROBE_ID`, whose answer [`settle_unsettled`]
/// judges). Nothing is sent again. Returns `false` when the core is gone.
fn answer_unsettled(
    shared: &Shared,
    pump: &EventSender,
    verb: crate::transport::UnsettledVerb,
    order_client_id: String,
    reason: &str,
    asked: AmendAsked,
) -> bool {
    let unknown = vike_bridge_core::transport::VenueApiError {
        code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
        msg: format!("dukascopy: {reason}"),
    };
    tracing::warn!(
        target: TARGET,
        %order_client_id,
        ?verb,
        %reason,
        "JForex answered a cancel or an amend neither way within the sidecar's bound: its outcome is unknown, and the \
         venue is read for it"
    );
    let answer = match verb {
        crate::transport::UnsettledVerb::Cancel => {
            lock(&shared.unsettled).cancels.insert(order_client_id.clone());
            vike_bridge_core::rest::cancel_rejected(&order_client_id, &unknown)
        }
        crate::transport::UnsettledVerb::Modify => {
            lock(&shared.unsettled).amends.insert(order_client_id.clone(), asked);
            vike_bridge_core::rest::modify_rejected(
                &order_client_id,
                &unknown,
                vike_bridge_core::rest::amend_answer_time(None),
            )
        }
    };
    if !shared.reconcile.probe() {
        tracing::warn!(
            target: TARGET,
            %order_client_id,
            "the venue cannot be read now (the jar does not announce `recon`, or the write failed): the sidecar's late \
             answer, the reconcile pass or the node's watchdog settles it"
        );
    }
    pump.blocking_send(answer).is_ok()
}

/// Whether a venue read shows that a cancel took: a protection's price is on none of the open orders facing it while
/// one of them still is open (a price that FIRED closed its order, and that close's message reports the fill); any other
/// order is listed in no state at all (one listed FILLED has filled, and its fill message reports it).
fn cancel_took(
    answer: &ReconcileAnswer,
    order_client_id: &str,
    protection: Option<&OrderRequest>,
) -> bool {
    let Some(request) = protection else {
        return !answer
            .orders
            .iter()
            .any(|o| o.order_client_id.as_deref() == Some(order_client_id));
    };
    let one = [(order_client_id.to_string(), request.clone())];
    let held = crate::reconcile::order_reports(answer, &one)
        .iter()
        .any(|report| report.order_client_id.as_deref() == Some(order_client_id));
    let facing_open = answer.orders.iter().any(|o| {
        o.state == "FILLED"
            && o.symbol.eq_ignore_ascii_case(&request.symbol)
            && o.side == -request.side
    });
    !held && facing_open
}

/// **The venue read [`answer_unsettled`] asked for answers** the cancels and amends whose outcome JForex left open
/// (decision 0303: an unknown outcome is resolved by a read keyed by the order client id, and nothing is sent again).
/// `answer` is `None` when the read failed: nothing changes, and the next answer under the probe's id reads again.
/// Returns `false` when the core is gone.
///
/// * **A cancel that took** ([`cancel_took`]) of an order the node still has in flight ends it `OrderCanceled`
///   ([`CANCEL_UNSETTLED_TOOK`]). Not shown to have taken: it stays open.
/// * **An amend** whose order the venue shows RESTING at every term it asked for ([`AmendAsked::read_back`]) took: the
///   node is told `OrderModified`, with the terms the venue shows. Not there (yet), or a term the read cannot show: it
///   stays open. The order no longer rests: its own events end it, and nothing more is said of the amend.
/// * The read is a snapshot taken right after the sidecar's bound, so what settles a change that lands later is JForex's
///   late `ORDER_CHANGED_OK` / `ORDER_CHANGED_REJECTED`, which the sidecar sends as the answer; what is still open is
///   read again at the next answer under the probe's id.
fn settle_unsettled(shared: &Shared, answer: Option<&ReconcileAnswer>, pump: &EventSender) -> bool {
    let Some(answer) = answer else { return true };
    let (cancels, amends): (Vec<String>, Vec<(String, AmendAsked)>) = {
        let open = lock(&shared.unsettled);
        (
            open.cancels.iter().cloned().collect(),
            open.amends.iter().map(|(id, asked)| (id.clone(), *asked)).collect(),
        )
    };
    for order_client_id in cancels {
        let in_flight = lock(&shared.underway).contains_key(&order_client_id);
        let protection = lock(&shared.protections).get(&order_client_id).cloned();
        if in_flight && !cancel_took(answer, &order_client_id, protection.as_ref()) {
            continue;
        }
        lock(&shared.unsettled).cancels.remove(&order_client_id);
        if !in_flight {
            continue;
        }
        lock(&shared.underway).remove(&order_client_id);
        lock(&shared.protections).remove(&order_client_id);
        lock(&shared.cancelling).remove(&order_client_id);
        tracing::info!(target: TARGET, %order_client_id, "the venue read shows the cancel JForex left unanswered took");
        let canceled = Event::OrderCanceled(OrderCanceled {
            order_client_id,
            reason: CANCEL_UNSETTLED_TOOK.into(),
            timestamp: vike_model::now_ms(),
            post_only_would_cross: false,
            refusal_id: None,
        });
        if pump.blocking_send(canceled).is_err() {
            return false;
        }
    }
    for (order_client_id, asked) in amends {
        let resting = lock(&shared.underway).contains_key(&order_client_id).then(|| {
            crate::reconcile::order_reports(answer, &[])
                .into_iter()
                .find(|report| report.order_client_id.as_deref() == Some(order_client_id.as_str()))
        });
        let Some(report) = resting.flatten() else {
            lock(&shared.unsettled).amends.remove(&order_client_id);
            continue;
        };
        if asked.read_back(&report) != Some(true) {
            continue;
        }
        lock(&shared.unsettled).amends.remove(&order_client_id);
        tracing::info!(target: TARGET, %order_client_id, "the venue read shows the amend JForex left unanswered took");
        let modified = Event::OrderModified(vike_model::events::OrderModified {
            order_client_id,
            venue_order_id: Some(report.venue_order_id),
            new_qty: asked.new_qty.map(|_| report.qty),
            new_price: asked.new_price.and(report.price),
            new_trigger_price: asked.new_trigger_price.and(report.trigger_price),
            timestamp: vike_bridge_core::rest::amend_answer_time(None),
        });
        if pump.blocking_send(modified).is_err() {
            return false;
        }
    }
    true
}

/// Live Dukascopy exec client — owns the sidecar child process, and starts the next one when it dies.
pub struct ExecutionClientDukascopy {
    /// The state every sidecar of this client shares (its stdin, the in-flight set, the netting shadow, the
    /// sidecar's state, the running sidecar's child and reader).
    shared: Arc<Shared>,
    events: EventSender,
    /// The supervisor thread (`supervise`): parked while a sidecar lives, restarting one while none does.
    supervisor: Option<std::thread::JoinHandle<()>>,
    /// Wakes the supervisor out of its park or its backoff wait; `Shared::stopping` is what it then reads.
    wake_supervisor: std_mpsc::Sender<()>,
    /// **The account identifier the FIRST sidecar's READY handshake carried** — `IAccount.getAccountId()`
    /// on the JForex account object this session authenticated as, verbatim.
    ///
    /// ⚠ Kept rather than discarded, which it was until 2026-09-15: `spawn_with_program` matched
    /// `Ok(Ok(_account))` and threw the venue's own answer away at the one moment it is knowable.
    /// It is what `crate::adapter`'s `record_confirmation` records so that `account.venue_account_id`
    /// can say *the venue confirmed it* rather than *somebody told the store* — the
    /// credential-schema spec's §4.5 handshake writer. A restarted sidecar logs in as the same account.
    ///
    /// ⚠ **Not a secret, and not a login either** — it is the id the venue prints on its own pages
    /// and echoes on its own wire, and it is already logged at `info` by the reader thread. The login
    /// and the password this client was spawned with reach the child through ENV; the one copy this
    /// client keeps is the supervisor's, to log in again, and it is never logged.
    handshake_account: String,
    /// The operator HALT sentinel this client watches — handed in by the start from
    /// `AdapterInputs::process.halt_path` ([`Self::with_halt_path`]), never resolved here.
    ///
    /// `None` (a test, a tool) watches NO file: this client used to fall back to the process-wide
    /// resolver (`vike_boot::halt::halt_path_from_env`), which made its kill switch work by
    /// reaching into process state at the moment an order arrived — a bridge reading a
    /// process-global path, which the start contract forbids (decision 0099). A client with no path
    /// says so once at `error` (`vike_execution::halt::sentinel_engaged`), because a start that
    /// forgot to hand it one is a dead kill switch.
    halt_path: Option<PathBuf>,
}

/// The child JVM's argv, built from the CALLER'S already-resolved [`DukascopyTools`].
///
/// A pure function rather than a literal inside [`ExecutionClientDukascopy::spawn`] so the argv can
/// be asserted without forking, and so the one ORDERING rule below has a place to be stated.
///
/// ⚠ **`-Duser.home=` goes BEFORE `-jar`, and that is not cosmetic.** The JVM treats everything up
/// to `-jar` as its OWN options and everything after `-jar <path>` as arguments to the application;
/// `java -jar bridge.jar -Duser.home=X` therefore hands the flag to `Bridge.main` — which ignores
/// it — and the JVM keeps `$HOME`. The bug looks exactly like the defect being fixed, so the test
/// that proves the flag lands is one that parses the argv the way a JVM does
/// (`crates/bridges/dukascopy/src/bin/fake_jforex_bridge.rs`'s `jvm_option`), never one that
/// searches the whole vector.
///
/// ⚠ **The flag is ONE argv element and is never quoted by us.** `std::process::Command` passes
/// each element to the OS as a unit — on Unix through `execvp`'s vector, on Windows through the
/// standard library's own command-line escaping — so a `user.home` holding spaces or a drive letter
/// (`C:\Program Files\vike trader\settings\state\jforex`) needs no shell quoting and must not be
/// given any. Joining these into a string and letting a shell split it is the way that breaks.
///
/// `None` emits NO home flag at all, which leaves the JVM's own `user.home` in place —
/// `crates/bridges/dukascopy/src/config.rs`'s `resolve_dukascopy_tools` argues why that rung is a
/// refusal rather than a guess. The heap flag ([`MAX_HEAP_FLAG`]) is unconditional and precedes
/// `-jar` for the same reason the home flag does.
fn jvm_args(tools: &DukascopyTools) -> Vec<String> {
    let mut args = Vec::with_capacity(4);
    if let Some(home) = tools.jforex_home.as_ref() {
        args.push(format!("{USER_HOME_FLAG}{}", home.display()));
    }
    args.push(MAX_HEAP_FLAG.to_string());
    args.push("-jar".to_string());
    args.push(tools.bridge_jar.display().to_string());
    args
}

impl ExecutionClientDukascopy {
    /// Spawn the real sidecar with the CALLER'S resolved tool paths, pass creds via child env
    /// (never argv), and wait for the ready/fatal handshake.
    ///
    /// `tools` comes from [`crate::resolve_dukascopy_tools`], called by the composition root that
    /// owns the environment sweep — see this module's doc for why this function resolves nothing
    /// itself. A jar that is not a file is the live gate: warn with the path that was actually
    /// tried (so an operator can see WHICH project was resolved), and the account does not run.
    pub fn spawn(
        config: DukascopyConfig,
        tools: &DukascopyTools,
        events: EventSender,
    ) -> Result<Self, DukascopyError> {
        Self::spawn_quoting(config, tools, events, QuoteBus::new())
    }

    /// [`Self::spawn`], with the sidecar's price lane attached to `quotes` (`crate::feed`): the production start
    /// passes the process's bus (`crate::feed::process_bus`), so the data lane's stream and this client share one
    /// sidecar. [`Self::spawn`] gives a private bus nobody listens to.
    pub fn spawn_quoting(
        config: DukascopyConfig,
        tools: &DukascopyTools,
        events: EventSender,
        quotes: Arc<QuoteBus>,
    ) -> Result<Self, DukascopyError> {
        if !tools.bridge_jar.is_file() {
            tracing::warn!(
                target: TARGET,
                jar = %tools.bridge_jar.display(),
                java = %tools.java,
                "bridge jar not found — the account does not run (install it at <project>/bin/jforex/, or name \
                 it with JFOREX_BRIDGE_JAR)"
            );
            return Err(DukascopyError::Unavailable(ErrorKind::Unknown));
        }
        Self::spawn_with_program_quoting(&tools.java, &jvm_args(tools), &config, events, quotes)
    }

    /// Test seam: spawn an arbitrary program speaking the bridge protocol, with the production
    /// retry policy ([`ATTEMPT_READY_TIMEOUT`] x [`MAX_SPAWN_ATTEMPTS`]).
    pub fn spawn_with_program(
        program: &str,
        args: &[String],
        config: &DukascopyConfig,
        events: EventSender,
    ) -> Result<Self, DukascopyError> {
        Self::spawn_with_program_quoting(program, args, config, events, QuoteBus::new())
    }

    /// [`Self::spawn_with_program`] with the price lane attached to `quotes` ([`Self::spawn_quoting`]).
    pub fn spawn_with_program_quoting(
        program: &str,
        args: &[String],
        config: &DukascopyConfig,
        events: EventSender,
        quotes: Arc<QuoteBus>,
    ) -> Result<Self, DukascopyError> {
        Self::spawn_with_attempts_quoting(
            program,
            args,
            config,
            events,
            quotes,
            ATTEMPT_READY_TIMEOUT,
            MAX_SPAWN_ATTEMPTS,
        )
    }

    /// [`Self::spawn_with_program`] with the retry policy as parameters, so a test can script a hung
    /// first start in milliseconds.
    ///
    /// ⚠ **A sidecar that sends neither `ready` nor `fatal` within `attempt_timeout` is KILLED and
    /// REAPED, then started again, up to `max_attempts` starts in all** (never past
    /// [`READY_TIMEOUT`] in total). Only that silence is retried: a `fatal`, a failed spawn and a
    /// child that dies without a word are verdicts and return at once. The old child is gone
    /// (`Child::wait` returned) before the next one starts, so one login never has two JForex
    /// sessions.
    pub fn spawn_with_attempts(
        program: &str,
        args: &[String],
        config: &DukascopyConfig,
        events: EventSender,
        attempt_timeout: Duration,
        max_attempts: u32,
    ) -> Result<Self, DukascopyError> {
        Self::spawn_with_attempts_quoting(
            program,
            args,
            config,
            events,
            QuoteBus::new(),
            attempt_timeout,
            max_attempts,
        )
    }

    /// [`Self::spawn_with_attempts`] with the price lane attached to `quotes` ([`Self::spawn_quoting`]); every attempt
    /// that reaches `ready` attaches to the same bus, and the reader detaches it at EOF.
    pub fn spawn_with_attempts_quoting(
        program: &str,
        args: &[String],
        config: &DukascopyConfig,
        events: EventSender,
        quotes: Arc<QuoteBus>,
        attempt_timeout: Duration,
        max_attempts: u32,
    ) -> Result<Self, DukascopyError> {
        Self::launch_supervised(
            Launch {
                program: program.to_string(),
                args: args.to_vec(),
                config: config.clone(),
                quotes,
                attempt_timeout,
                liveness: LIVENESS,
            },
            events,
            max_attempts,
            RESTART,
        )
    }

    /// [`Self::spawn_with_program`] with the restart policy as a parameter, so a test can script a sidecar that dies,
    /// is refused a few times and comes back in milliseconds (the production policy is [`RESTART`]).
    pub fn spawn_with_restart(
        program: &str,
        args: &[String],
        config: &DukascopyConfig,
        events: EventSender,
        restart: VenueTimingsReconnect,
    ) -> Result<Self, DukascopyError> {
        Self::spawn_with_liveness(
            program,
            args,
            config,
            events,
            restart,
            venue_timings::DUKASCOPY.exec(),
        )
    }

    /// [`Self::spawn_with_restart`] with the living sidecar's probe and idle limit as a lane (its `keepalive_every` and
    /// `idle_threshold`, read through the accessors: a lane that declares either not panics), so a test can script a
    /// sidecar that wedges and is killed in milliseconds (the production bounds are [`LIVENESS`]).
    pub fn spawn_with_liveness(
        program: &str,
        args: &[String],
        config: &DukascopyConfig,
        events: EventSender,
        restart: VenueTimingsReconnect,
        liveness: venue_timings::VenueTimingsLane,
    ) -> Result<Self, DukascopyError> {
        Self::launch_supervised(
            Launch {
                program: program.to_string(),
                args: args.to_vec(),
                config: config.clone(),
                quotes: QuoteBus::new(),
                attempt_timeout: ATTEMPT_READY_TIMEOUT,
                liveness: Liveness::of(liveness),
            },
            events,
            MAX_SPAWN_ATTEMPTS,
            restart,
        )
    }

    /// The first start (up to `max_attempts` sidecars, a silent one killed and replaced), then the supervisor that
    /// starts every later one. A first start that fails is the live gate: no client, no supervisor.
    fn launch_supervised(
        launch: Launch,
        events: EventSender,
        max_attempts: u32,
        restart: VenueTimingsReconnect,
    ) -> Result<Self, DukascopyError> {
        let stdin: Arc<Stdin> = Arc::new(Mutex::new(None));
        let shared = Arc::new(Shared {
            events: events.clone(),
            stdin: Arc::clone(&stdin),
            underway: Mutex::new(HashMap::new()),
            stranded: Mutex::new(HashSet::new()),
            reconcile: ReconcileLink::new(
                stdin as Arc<dyn LinePort>,
                launch.config.ledger_mount.as_deref(),
            ),
            shadow: Mutex::new(PositionsShadow::default()),
            // The cell outlives its clients (it is the account's): a new client claims it, so it starts from "not known
            // yet", never from the last one's outage, and reads up once its first sidecar has said `ready`.
            said: Mutex::new(VenueLinkState::Unknown),
            link: events.route_key.map_or_else(VenueLink::default, |key| venue_link(&key)).claim(),
            last_fatal: Mutex::new(None),
            session: Mutex::new(None),
            amend_supported: AtomicBool::new(false),
            adopts: AtomicBool::new(false),
            take_profit_supported: AtomicBool::new(false),
            trigger_entry_supported: AtomicBool::new(false),
            starts: AtomicUsize::new(0),
            protections: Mutex::new(HashMap::new()),
            cancelling: Mutex::new(HashSet::new()),
            unanswered: Mutex::new(HashSet::new()),
            stopping: AtomicBool::new(false),
            restart,
            gaps: Mutex::new(GapWindow::new(Instant::now())),
            heard: Mutex::new(None),
            unsettled: Mutex::new(Unsettled::default()),
        });
        let (wake, parked) = std_mpsc::channel::<()>();
        let began = Instant::now();
        let first = 'start: {
            for attempt in 1..=max_attempts {
                // The whole start never outlives `READY_TIMEOUT`, whatever the caller asked for.
                let Some(wait) = READY_TIMEOUT
                    .checked_sub(began.elapsed())
                    .filter(|left| !left.is_zero())
                    .map(|left| left.min(launch.attempt_timeout))
                else {
                    break;
                };
                match Self::start_sidecar(&launch, &shared, &wake, wait) {
                    Ok(started) => break 'start started,
                    // A verdict (fatal, spawn error, silent death): a respawn would only repeat it. Its words go on
                    // the cell: `DukascopyError` carries only their kind, and the gatewayhub's refusal reads them there.
                    Err(AttemptFailed::Refused(why, kind)) => {
                        shared.set_state(sidecar_down(why, 0, Duration::ZERO));
                        return Err(DukascopyError::Unavailable(kind));
                    }
                    // `start_sidecar` has already killed AND reaped the child: no second JForex session
                    // ever shares the login with a hung first one.
                    Err(AttemptFailed::NoReady) => tracing::warn!(
                        target: TARGET,
                        attempt,
                        max_attempts,
                        waited_secs = wait.as_secs(),
                        "the JForex sidecar sent no ready in time — killed it"
                    ),
                }
            }
            shared.set_state(sidecar_down(
                format!(
                    "it sent no ready in {} s, and every sidecar tried was killed",
                    began.elapsed().as_secs()
                ),
                0,
                Duration::ZERO,
            ));
            // A login that hung (2026-10-17) or a fresh cache still downloading: a later start can pass.
            return Err(DukascopyError::Unavailable(ErrorKind::Timeout));
        };
        // The first sidecar said `ready`: up (E18), unless its reader has already said it went down since.
        {
            let mut said = lock(&shared.said);
            if *said == VenueLinkState::Unknown {
                shared.link.set(VenueLinkState::Up, None);
                *said = VenueLinkState::Up;
            }
        }
        shared.amend_supported.store(first.amend_supported, Ordering::SeqCst);
        *lock(&shared.session) = Some(first.session);
        let supervisor = {
            let shared = Arc::clone(&shared);
            let wake = wake.clone();
            std::thread::Builder::new()
                .name("dukascopy-sidecar-supervisor".into())
                .spawn(move || {
                    // A panic here (a bug) ends the restarts and nothing else: the sidecar that runs keeps serving, and
                    // the account's link says nothing restarts it (decision 0302: a dead thread is noticed), unless the
                    // client is stopping, whose cell may be the next start's. `AssertUnwindSafe`: only the flag and the
                    // link are read after it.
                    let supervised = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        supervise(&launch, &shared, &wake, &parked)
                    }));
                    if supervised.is_err() && !shared.is_stopping() {
                        tracing::error!(target: TARGET, "{SUPERVISOR_PANICKED}: {SUPERVISOR_EFFECT}");
                        let down = VenueLinkDown::failed(
                            SUPERVISOR_PANICKED.to_string(),
                            SUPERVISOR_EFFECT,
                            0,
                            Duration::ZERO,
                        );
                        shared.link.claim_path(SUPERVISOR_PATH).gave_up(down);
                    }
                })
                .expect("spawn supervisor thread")
        };
        Ok(Self {
            shared,
            events,
            supervisor: Some(supervisor),
            wake_supervisor: wake,
            handshake_account: first.account,
            halt_path: None,
        })
    }

    /// ONE sidecar start: spawn the child, wait up to `wait` for its handshake, and on anything but
    /// a READY leave NO child behind (killed and reaped, reader joined). On a READY the child's stdin is the
    /// client's (`Shared::stdin`) and its reader runs; the caller installs the [`SidecarProcess`].
    fn start_sidecar(
        launch: &Launch,
        shared: &Arc<Shared>,
        wake: &std_mpsc::Sender<()>,
        wait: Duration,
    ) -> Result<Started, AttemptFailed> {
        let config = &launch.config;
        let quotes = &launch.quotes;
        let mut child = std::process::Command::new(&launch.program)
            .args(&launch.args)
            .env("DUKASCOPY_LOGIN", &config.login)
            .env("DUKASCOPY_PASSWORD", config.password.expose())
            .env("DUKASCOPY_JNLP", &launch.config.server)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                tracing::error!(target: TARGET, error = %e, "failed to spawn bridge");
                // No `java` (or not runnable): not the venue's failure, so it parks.
                AttemptFailed::Refused(
                    format!("the sidecar process could not be started: {e}"),
                    ErrorKind::Unknown,
                )
            })?;

        *lock(&shared.stdin) = Some(child.stdin.take().expect("piped stdin"));
        let stdout = child.stdout.take().expect("piped stdout");

        // Handshake channel: the reader thread forwards the first ready/fatal here,
        // then pumps every subsequent `event` envelope into the core ingest.
        let (hs_tx, hs_rx) =
            std_mpsc::sync_channel::<Result<(String, bool), (String, ErrorKind)>>(1);
        let quotes_reader = Arc::clone(quotes);
        let ledger_mount = config.ledger_mount.clone();
        let shared_reader = Arc::clone(shared);
        let ended = wake.clone();
        let reader = std::thread::Builder::new()
            .name("dukascopy-bridge-reader".into())
            .spawn(move || {
                let pump = shared_reader.events.clone();
                let reader_stdin = Arc::clone(&shared_reader.stdin);
                let mut handshaken = false;
                // The price lane's attach id (`crate::feed`), set on `ready` and given back at EOF by THIS thread,
                // so a tick is never seen before the attach nor after the detach.
                let mut attached: Option<u64> = None;
                // Set when the core stopped listening (`blocking_send` failed): nobody is left to serve.
                let mut core_gone = false;
                // The protections sent to this sidecar again at its start: their answer's `OrderAccepted` is not a second
                // acceptance (the node holds them accepted), and a terminal clears them.
                let mut replaying: HashSet<String> = HashSet::new();
                // The protections whose cancel was in flight at the last sidecar's death, held back until this sidecar
                // has read the venue (`settle_cancels_in_flight`).
                let mut awaiting: Vec<(String, OrderRequest)> = Vec::new();
                // The symbols this sidecar sent a `position` line for between its `ready` and its `adopted` census
                // (`Some` only for a jar that adopts): the census ends them, and a symbol the node holds a position in
                // that is not among them is flat at the venue.
                let mut census: Option<HashSet<String>> = None;
                // The symbols whose `position` line this sidecar's shadow adopted as its baseline: the venue's position,
                // a fill the dead sidecar never reported included, which the venue read must then not fold again
                // (`book_read_fill`).
                let mut baselines: HashSet<String> = HashSet::new();
                // Which sidecar of this client this is (1 = the first).
                let mut start_no = 0usize;
                // This sidecar's row in the session history, from its `ready`. A drop at a clean stop is ours.
                let mut venue_session = None;
                // This sidecar's gaps start over: an outage before it is the DOWN/BACK lines', not an idle gap.
                *lock(&shared_reader.gaps) = GapWindow::new(Instant::now());
                // Its silence counts from its `ready`, never before: the handshake's own bounds govern the start.
                *lock(&shared_reader.heard) = None;
                // A panic in a handler below (a bug) ends this sidecar as its death does (decision 0302: a dead thread is
                // noticed): its stdin is closed, which ends it and refuses every write from here, and the drain below
                // answers what was in flight; the supervisor reaps it and starts the next one. `AssertUnwindSafe`: what
                // the loop held is read again only by that drain, as after any death.
                let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    for line in BufReader::new(stdout).lines() {
                        let Ok(line) = line else { break };
                        let now = Instant::now();
                        lock(&shared_reader.gaps).note(now);
                        if let Some(heard) = lock(&shared_reader.heard).as_mut() {
                            *heard = now;
                        }
                        match parse_envelope(&line) {
                            Some(Envelope::Ready { account, balance, features, currency }) if !handshaken => {
                                handshaken = true;
                                venue_session = Some(vike_bridge_core::link_history::VenueLinkRecord::of(
                                    &shared_reader.events,
                                    "sidecar",
                                ));
                                tracing::info!(
                                    target: TARGET, %account, balance, ?features, ?currency, "bridge ready"
                                );
                                let can_amend = features.iter().any(|f| f == FEATURE_MODIFY);
                                let adopts = features.iter().any(|f| f == FEATURE_ADOPT);
                                shared_reader.adopts.store(adopts, Ordering::SeqCst);
                                shared_reader.take_profit_supported.store(
                                    features.iter().any(|f| f == FEATURE_TAKE_PROFIT),
                                    Ordering::SeqCst,
                                );
                                shared_reader.trigger_entry_supported.store(
                                    features.iter().any(|f| f == FEATURE_TRIGGER_ENTRY),
                                    Ordering::SeqCst,
                                );
                                // Before the start is told it succeeded: the adapter reads it to decide whether the node gets a
                                // reconcile client for this jar.
                                let answers_recon = features.iter().any(|f| f == FEATURE_RECONCILE);
                                shared_reader.reconcile.set_supported(answers_recon);
                                // The supervisor's liveness probe IS the `recon` question, so a jar that cannot answer it
                                // gets neither the probe nor the idle limit.
                                *lock(&shared_reader.heard) = answers_recon.then_some(now);
                                if !answers_recon {
                                    tracing::info!(
                                        target: TARGET,
                                        "the installed JForex sidecar jar does not announce `recon`: no liveness probe and no \
                                         idle limit for this sidecar (only its death is seen and restarted)"
                                    );
                                }
                                start_no = shared_reader.starts.fetch_add(1, Ordering::SeqCst) + 1;
                                census = adopts.then(HashSet::new);
                                let id = quotes_reader.attach(
                                    Arc::clone(&reader_stdin) as Arc<dyn LinePort>,
                                    &features,
                                );
                                attached = Some(id);
                                // The account's currency (decision 0183) is on the bus BEFORE the start is told it
                                // succeeded, so the trade lane's first read of it finds it.
                                quotes_reader.set_account_currency(id, currency.as_deref());
                                // The resting protections the last sidecar's death left with no owner go to this one BEFORE
                                // the start is told it succeeded, so no command of the node's can reach it ahead of them.
                                if !hand_back_protections(
                                    &shared_reader,
                                    adopts,
                                    &pump,
                                    &mut replaying,
                                    &mut awaiting,
                                ) {
                                    core_gone = true; // core gone — stop pumping
                                    break;
                                }
                                let _ = hs_tx.send(Ok((account, can_amend)));
                                // The engine's only way to learn the venue's balance on this unreconciled
                                // venue (`crate::account::ready_balance`): ONE wallet frame per sidecar start, sent after
                                // the handshake reply so a stalled lane can never cost the start itself, and
                                // only for a start that was told its mount. The currency is the bus's own
                                // reading of the line, so the gate and the node's ledger agree on it.
                                if let Some(mount) = ledger_mount.as_deref() {
                                    let stated = quotes_reader.account_currency();
                                    let now = vike_model::now_ms();
                                    // `wallet_frame` says on its own whether it built one and, if not, why.
                                    if let Ok(frame) = wallet_frame(balance, stated.as_deref(), mount, now)
                                        && pump.blocking_send(frame).is_err()
                                    {
                                        core_gone = true; // core gone — stop pumping
                                        break;
                                    }
                                }
                            }
                            Some(Envelope::Tick { symbol, bid, ask, time }) => {
                                // No tick precedes `ready` on a protocol-honest sidecar (and none is sent before a
                                // `subscribe`), so one that does is dropped like any pre-ready line. `deliver` only
                                // queues for the consumer: a fault there never reaches this thread.
                                if handshaken {
                                    quotes_reader.deliver(&symbol, bid, ask, time, vike_model::now_ms());
                                }
                            }
                            Some(Envelope::Subscription { symbol, ok, reason }) => {
                                // The jar's answer to a `subscribe` (only a jar that announced `tick-acks` sends one).
                                if handshaken {
                                    quotes_reader.subscription(&symbol, ok, &reason);
                                }
                            }
                            Some(Envelope::Terms(terms)) => {
                                // The venue's own terms of an instrument (only a jar that announced `terms` sends one),
                                // kept on the bus for the admission of a metal or a CFD (`QuoteBus::venue_terms`).
                                if handshaken {
                                    quotes_reader.terms(terms);
                                }
                            }
                            Some(Envelope::Instruments { instruments }) => {
                                // The account's instrument list, stated once per login just BEFORE `ready` (the one line
                                // taken before the handshake: it moves no order), so the start is told it succeeded with the
                                // list already on the bus. Every grid, class and conversion pair of this venue is read from
                                // it (`QuoteBus::instrument_list`), and the gatewayhub's catalog hands the same rows to the
                                // datahub (`crate::catalog::hold`). An empty statement replaces nothing (the bus says so).
                                if !instruments.is_empty() {
                                    crate::catalog::hold(&instruments);
                                }
                                quotes_reader.set_instrument_list(instruments);
                            }
                            Some(Envelope::Ready { .. }) => {} // duplicate ready: ignore
                            Some(Envelope::Fatal { reason, class, failure, message }) => {
                                tracing::error!(target: TARGET, %reason, class = class.as_deref(), "bridge fatal");
                                if !handshaken {
                                    let kind = fatal_kind(
                                        &reason,
                                        class.as_deref(),
                                        failure.as_deref(),
                                        message.as_deref(),
                                    );
                                    let _ = hs_tx.send(Err((reason, kind)));
                                } else {
                                    // the words of the death, for `Shared::went_down`
                                    *lock(&shared_reader.last_fatal) = Some(reason);
                                }
                                // post-ready fatal precedes sidecar exit → EOF ends the loop
                            }
                            Some(Envelope::Event { mut event }) => {
                                if !handshaken {
                                    // Protocol: no event may precede the ready handshake —
                                    // a ghost event here would reach the core for an order
                                    // it never submitted (and pre-ready blocking_send could
                                    // wedge the not-yet-live ingest lane).
                                    tracing::debug!(target: TARGET, ?event, "dropped pre-ready event");
                                    continue;
                                }
                                // A fill names the instrument by its wire name: booked under its user name (decision 0290).
                                fill_by_user_name(&mut event);
                                // The answer to a protection sent again at the start is the take-over of a price the node
                                // already holds accepted: not a second acceptance.
                                if let Event::OrderAccepted(accepted) = &*event
                                    && replaying.remove(&accepted.order_client_id)
                                {
                                    continue;
                                }
                                // An amend's answer carries the time it was received here: the sidecar's line echoes the
                                // stamp of its `SidecarCommand::Modify`, which is the order's own creation time.
                                match &mut *event {
                                    Event::OrderModified(m) => {
                                        m.timestamp = vike_bridge_core::rest::amend_answer_time(None);
                                    }
                                    Event::OrderModifyRejected(r) => {
                                        r.timestamp = vike_bridge_core::rest::amend_answer_time(None);
                                    }
                                    _ => {}
                                }
                                // An amended protection is a new price at the venue: the copy kept to send again follows it.
                                if let Event::OrderModified(modified) = &*event
                                    && let Some(price) = modified.new_price
                                    && let Some(kept) =
                                        lock(&shared_reader.protections).get_mut(&modified.order_client_id)
                                {
                                    if kept.order_type == vike_model::OrderType::Stop {
                                        kept.trigger_price = Some(price);
                                    } else {
                                        kept.price = Some(price);
                                    }
                                }
                                // An end the core can fold for an order the sidecar acknowledged, and an honest one for
                                // a reduce-only close filled short (`settle`); `then` is the terminal that follows.
                                let (event, then) = settle(&mut lock(&shared_reader.underway), *event);
                                // A liquidation ends this venue's order too, so the tracking closes on `closed_order`.
                                if let Some(order_client_id) = then.as_ref().unwrap_or(&event).closed_order() {
                                    lock(&shared_reader.underway).remove(order_client_id);
                                    lock(&shared_reader.protections).remove(order_client_id);
                                    lock(&shared_reader.cancelling).remove(order_client_id);
                                    replaying.remove(order_client_id);
                                    lock(&shared_reader.unsettled).forget(order_client_id);
                                }
                                // A refused cancel is answered: it is no longer in flight, and the order stays as it was.
                                if let Event::OrderCancelRejected(refused) = &event {
                                    lock(&shared_reader.cancelling).remove(&refused.order_client_id);
                                }
                                // JForex's late answer (or the answer to a newer command) settles a cancel or an amend
                                // whose outcome it had left open.
                                lock(&shared_reader.unsettled).answered_by(&event);
                                // Shadow-fold the bare fill lane ONLY (the OrderFilled wrap carries
                                // the same fill and must not double-fold — same law as the Account).
                                if let Event::Fill(fill) = &event {
                                    lock(&shared_reader.shadow).fold_fill(fill);
                                    // A symbol the node may now hold a position in gets a flat row at the reconcile query.
                                    shared_reader.reconcile.know(&fill.symbol);
                                }
                                if std::iter::once(event).chain(then).any(|e| pump.blocking_send(e).is_err()) {
                                    core_gone = true; // core gone — stop pumping
                                    break;
                                }
                            }
                            Some(Envelope::Position { symbol, size, average_price: avg_px, timestamp: ts }) => {
                                if !handshaken {
                                    tracing::debug!(target: TARGET, %symbol, "dropped pre-ready position line");
                                    continue;
                                }
                                // The line names the instrument by its wire name; the shadow and the reconcile's flat rows
                                // key it by its user name, as the fills are booked (decision 0290).
                                let symbol = crate::instrument::names::user_symbol_or_wire(&symbol);
                                // The reconcile query reads the venue itself (`crate::reconcile::query`); a `position` line only
                                // tells it which symbols to state flat.
                                shared_reader.reconcile.know(&symbol);
                                // A line between `ready` and the `adopted` census is the start's: the census says which
                                // symbols the venue listed an order on.
                                if let Some(seen) = census.as_mut() {
                                    seen.insert(symbol.clone());
                                }
                                // The shadow is the CLIENT's, not this sidecar's (`Shared::shadow`): a restart must not
                                // hand the next sidecar an empty one.
                                let resolved = lock(&shared_reader.shadow).reanchor(&symbol, size, avg_px, ts);
                                if resolved == PositionsShadowReanchor::Baseline {
                                    baselines.insert(symbol.clone());
                                }
                                match resolved {
                                    PositionsShadowReanchor::Corrected(legs) => {
                                        // Netted-close attribution drift: fold the venue truth in via
                                        // ordinary synthesized fills (see execution/netting.rs module doc).
                                        tracing::info!(
                                            target: TARGET,
                                            %symbol, venue_size = size, venue_average = avg_px,
                                            "netting re-anchor: folding venue basis via synthesized close+reopen legs"
                                        );
                                        if legs.into_iter().any(|leg| pump.blocking_send(leg).is_err())
                                        {
                                            core_gone = true; // core gone — stop pumping
                                            break;
                                        }
                                    }
                                    PositionsShadowReanchor::SizeMismatch { local_size, venue_size } => {
                                        // Never auto-healed here — a missed/spurious fill is real
                                        // recon territory; keep it LOUD until fills explain it.
                                        tracing::warn!(
                                            target: TARGET,
                                            %symbol, local_size, venue_size,
                                            "sidecar position line disagrees with folded fills — NOT auto-healed"
                                        );
                                    }
                                    PositionsShadowReanchor::Baseline if start_no > 1 && size.abs() > 1e-3 => {
                                        // A restarted sidecar (the client has lived since before it) lists a position in a
                                        // symbol no fill of this client ever folded: the venue holds something the node was
                                        // never told of (a market entry the dead sidecar had taken up and the venue filled).
                                        tracing::warn!(
                                            target: TARGET,
                                            %symbol, venue_size = size, venue_average = avg_px,
                                            "the venue holds a position that no fill folded by this client explains (a fill the \
                                             dead sidecar never reported?); adopted as the shadow's baseline, NOT booked here (the \
                                             venue read that follows books the fill of an order the dead sidecar left with no \
                                             end): compare `position ls` with the JForex platform"
                                        );
                                    }
                                    PositionsShadowReanchor::Baseline => {
                                        tracing::info!(
                                            target: TARGET,
                                            %symbol, venue_size = size, venue_average = avg_px,
                                            "adopted venue position line as shadow baseline (no folded fills yet)"
                                        );
                                    }
                                    PositionsShadowReanchor::InSync => {}
                                }
                            }
                            Some(Envelope::Adopted { orders, foreign }) => {
                                if !handshaken {
                                    tracing::debug!(target: TARGET, "dropped pre-ready adoption census");
                                    continue;
                                }
                                // Only the census of a jar that announced `adopt` (and only the first of its start) counts.
                                let Some(seen) = census.take() else {
                                    tracing::debug!(target: TARGET, "ignored an adoption census this start did not announce");
                                    continue;
                                };
                                // What the new sidecar took up it can cancel, amend and hear again: it is no longer stranded.
                                {
                                    let mut stranded = lock(&shared_reader.stranded);
                                    for adopted in &orders {
                                        stranded.remove(&adopted.order_client_id);
                                    }
                                }
                                let protected = orders
                                    .iter()
                                    .filter(|o| o.stop_loss > 0.0 || o.take_profit > 0.0)
                                    .count();
                                tracing::info!(
                                    target: TARGET,
                                    adopted = orders.len(),
                                    foreign,
                                    protected,
                                    symbols = seen.len(),
                                    "the sidecar adopted the venue's open orders: it can cancel, amend and hear them again"
                                );
                                // The census ended the start's position lines: a symbol the node's fills hold a position in
                                // and the venue listed no order on is FLAT at the venue (a close the dead sidecar never
                                // heard: a stop-loss that fired, a close in the platform). The shadow says so loudly and
                                // changes nothing (a size mismatch is never auto-healed, `execution/netting.rs`).
                                let open = lock(&shared_reader.shadow).open_symbols(); // (the guard must not outlive this line)
                                for symbol in open {
                                    if seen.contains(&symbol) {
                                        continue;
                                    }
                                    let resolved = lock(&shared_reader.shadow).reanchor(
                                        &symbol,
                                        0.0,
                                        0.0,
                                        vike_model::now_ms(),
                                    );
                                    if let PositionsShadowReanchor::SizeMismatch { local_size, venue_size } = resolved {
                                        tracing::warn!(
                                            target: TARGET,
                                            %symbol, local_size, venue_size,
                                            "at its start the sidecar listed NO order on {symbol}, but the node's folded fills \
                                             hold a position in it: the venue closed it while no sidecar listened (a stop-loss \
                                             that fired, a close in the platform). NOT auto-healed: check the JForex platform, \
                                             then flatten it (the venue refuses by name and the core holds an \
                                             OrphanLocalPosition row for `vike-cli trade reconcile confirm`) or cycle the \
                                             account (crates/bridges/dukascopy/CLAUDE.md, the phantom)"
                                        );
                                    }
                                }
                            }
                            Some(Envelope::Recon { id, orders, positions, foreign, timestamp: ts, error }) => {
                                if !handshaken {
                                    tracing::debug!(target: TARGET, id, "dropped pre-ready recon answer");
                                    continue;
                                }
                                // The venue read `hand_back_protections` or `answer_unsettled` asked for: no reconcile
                                // asker waits on the probe's id.
                                if id == crate::reconcile::query::PROBE_ID
                                    && (!awaiting.is_empty()
                                        || !lock(&shared_reader.unanswered).is_empty()
                                        || !lock(&shared_reader.unsettled).is_empty())
                                {
                                    let read = crate::reconcile::query::judge(orders, positions, foreign, ts, error)
                                        .map(crate::reconcile::in_user_names);
                                    if !settle_unanswered(
                                        &shared_reader,
                                        read.as_ref().ok(),
                                        &pump,
                                        &baselines,
                                    ) {
                                        core_gone = true; // core gone — stop pumping
                                        break;
                                    }
                                    if !settle_cancels_in_flight(
                                        &shared_reader,
                                        &read,
                                        &mut awaiting,
                                        &mut replaying,
                                        &pump,
                                    ) {
                                        core_gone = true; // core gone — stop pumping
                                        break;
                                    }
                                    // After the cancels in flight at a death: one it ended is no longer in flight here.
                                    if !settle_unsettled(&shared_reader, read.as_ref().ok(), &pump) {
                                        core_gone = true; // core gone — stop pumping
                                        break;
                                    }
                                    continue;
                                }
                                // The answer to a reconcile question (only a jar that announced `recon` sends one); the asker
                                // is parked on `ReconcileLink::ask`, and a late answer to a question that timed out is dropped.
                                if !shared_reader.reconcile.deliver(id, orders, positions, foreign, ts, error) {
                                    tracing::debug!(target: TARGET, id, "dropped a recon answer nobody waits for");
                                }
                            }
                            Some(Envelope::Unsettled {
                                verb,
                                order_client_id,
                                reason,
                                new_qty,
                                new_price,
                                new_trigger_price,
                            }) => {
                                if !handshaken {
                                    tracing::debug!(target: TARGET, %order_client_id, "dropped pre-ready unsettled line");
                                    continue;
                                }
                                let asked = AmendAsked { new_qty, new_price, new_trigger_price };
                                if !answer_unsettled(&shared_reader, &pump, verb, order_client_id, &reason, asked) {
                                    core_gone = true; // core gone — stop pumping
                                    break;
                                }
                            }
                            None => {
                                let snippet: String = line.chars().take(160).collect();
                                tracing::debug!(target: TARGET, %snippet, "skipped unparseable stdout line");
                            }
                        }
                    }
                }));
                if read.is_err() {
                    tracing::error!(target: TARGET, "{READER_PANICKED}");
                    *lock(&shared_reader.stdin) = None;
                    *lock(&shared_reader.last_fatal) = Some(READER_PANICKED.to_string());
                }
                // EOF: no sidecar answers a reconcile question any more; every open one fails at once, in words.
                shared_reader.reconcile.set_supported(false);
                shared_reader.reconcile.fail_all("the JForex sidecar ended while the reconcile query was open");
                // EOF: the price lane loses its sidecar (the wants stay, for the next one).
                if let Some(id) = attached {
                    quotes_reader.detach(id);
                }
                // EOF: a clean stop, a death, or a core that went away. A sidecar that had said `ready` and
                // ended while the client runs is DOWN: say so first, so a submit that races the drain below is
                // refused by name instead of entering a dead pipe.
                if core_gone {
                    shared_reader.stopping.store(true, Ordering::SeqCst);
                }
                let died = handshaken && !shared_reader.is_stopping();
                if died {
                    shared_reader.went_down();
                    if let (Some(session), VenueLinkState::Down(VenueLinkDown { why, .. })) =
                        (venue_session.take(), shared_reader.state())
                    {
                        session.ended(&why);
                    }
                }
                // Any order still awaiting its terminal event would otherwise wedge non-terminal (a later
                // failed stdin write only covers NEW submits) — end it now, except an order the sidecar never
                // answered, at a death, a clean stop or a start that never said `ready` alike (its submit was written,
                // so its outcome is unknown: held for a venue read or left to the stuck-order ladder).
                // EXACTLY ONCE: the drain and `submit`'s failed write both take an order client id out of the set
                // before ending it, so whoever finds it there ends it and the other finds nothing.
                let orphans: Vec<(String, Underway)> = lock(&shared_reader.underway).drain().collect();
                // A sidecar that ADOPTS is followed by one that does too (the same jar, started by the same
                // supervisor), which takes over the price a resting protection already carries at the venue when it is
                // sent again at the next start (`hand_back_protections`): the node keeps believing in the protection
                // the venue provably holds, and the next start tells it when the venue does not. It reads the venue for
                // an order this one never answered too (`settle_unanswered`). A start that never said `ready` (a
                // refused login on the restart ladder; `adopts` is still the dead sidecar's) read nothing: what the
                // last death held for that read stays held for the next start. Only a stop ends the ladder.
                let keeps_protections =
                    !shared_reader.is_stopping() && shared_reader.adopts.load(Ordering::SeqCst);
                for (order_client_id, underway) in orphans {
                    if keeps_protections
                        && underway.is_protection()
                        && underway.acknowledged
                        && lock(&shared_reader.protections).contains_key(&order_client_id)
                    {
                        tracing::warn!(
                            target: TARGET,
                            %order_client_id,
                            kind = ?underway.kind,
                            "bridge died with a resting protection — kept as it is on the node: the venue holds its \
                             price and the next sidecar takes it over"
                        );
                        continue;
                    }
                    if died {
                        let mut stranded = lock(&shared_reader.stranded);
                        if stranded.len() < MAX_STRANDED {
                            stranded.insert(order_client_id.clone());
                        }
                    }
                    // A submit the sidecar never answered may have reached the venue: its outcome is UNKNOWN, so it is
                    // never rejected here (decision 0303). An adopting jar's next sidecar reads the venue for it
                    // (`settle_unanswered`); any other jar, and a clean stop, leaves it `Submitted` for the core's
                    // stuck-order ladder (its last resort is the end, decision 0171) and reconcile.
                    // A market entry the adopting jar had acknowledged and reported no fill of may have FILLED: the same
                    // read ends it by what the venue lists under its label. One with a fill already reported (a partial,
                    // a net-close leg) keeps the cancel below: the read cannot say which part of the venue's row is new.
                    let entry_unheard = keeps_protections
                        && underway.kind == UnderwayKind::Entry
                        && underway.filled <= 0.0;
                    if !underway.acknowledged || entry_unheard {
                        tracing::warn!(
                            target: TARGET,
                            %order_client_id,
                            kind = ?underway.kind,
                            "the sidecar ended with an order whose end it never reported: its outcome is unknown, so \
                             it is NOT ended here; {}",
                            if keeps_protections {
                                "the next sidecar reads the venue for it"
                            } else {
                                "the node's stuck-order watchdog settles it"
                            }
                        );
                        if keeps_protections {
                            lock(&shared_reader.unanswered).insert(order_client_id.clone());
                            lock(&shared_reader.underway).insert(order_client_id, underway);
                        } else {
                            lock(&shared_reader.protections).remove(&order_client_id);
                            lock(&shared_reader.cancelling).remove(&order_client_id);
                        }
                        continue;
                    }
                    lock(&shared_reader.protections).remove(&order_client_id);
                    lock(&shared_reader.cancelling).remove(&order_client_id);
                    // A stop-loss the sidecar had ACCEPTED rests at the venue, and a market order (a reduce-only close
                    // or an entry) it had taken up may have been executed there; the core folds a rejection only from
                    // `Initialized`/`Submitted` (it would drop this one and keep the order `Accepted` for ever): each
                    // ends with a cancel, in words. NOTHING is sent to the venue; this is the node's belief only.
                    let cancels = underway.cancel_words_at_death();
                    tracing::warn!(
                        target: TARGET,
                        %order_client_id,
                        kind = ?underway.kind,
                        "bridge died with in-flight order — ending it in the node's books ({})",
                        if cancels.is_some() { "OrderCanceled" } else { "OrderRejected" }
                    );
                    let ended = match cancels {
                        Some(words) => Event::OrderCanceled(OrderCanceled {
                            order_client_id,
                            reason: words.into(),
                            timestamp: vike_model::now_ms(),
                            post_only_would_cross: false,
                            refusal_id: None,
                        }),
                        None => Event::OrderRejected(OrderRejected { cause: None,
                            order_client_id,
                            reason: BRIDGE_DIED.into(),
                            timestamp: 0,
                            refusal_id: vike_model::events::refusal_id_of(
                                vike_model::OrderRefusalId::OutcomeUnknown.word(),
                            ),
                        }),
                    };
                    let _ = pump.blocking_send(ended);
                }
                // The supervisor starts the next sidecar. A start that never said `ready` is
                // `start_sidecar`'s outcome to report, not a death: it tells nobody here.
                if handshaken {
                    let _ = ended.send(());
                }
            })
            .expect("spawn reader thread");

        // Ok(ready) | Err(fatal) | RecvTimeout (hang) | Disconnected (silent death). Waited in short slices so
        // that a client stopped meanwhile (a detach during a restart) does not wait for a login.
        let waiting = Instant::now();
        let handshake = loop {
            let left = wait.saturating_sub(waiting.elapsed());
            match hs_rx.recv_timeout(left.min(HANDSHAKE_POLL)) {
                Err(std_mpsc::RecvTimeoutError::Timeout)
                    if !left.is_zero() && !shared.is_stopping() => {}
                other => break other,
            }
        };
        match handshake {
            Ok(Ok((account, amend_supported))) => {
                // ⚠ This used to resolve — and, once per process, REPORT — the HALT sentinel here
                // (`vike_boot::halt::halt_path_from_env`), so an unarmable switch landed in
                // the trace file at START time. The resolver is the composition layer's now:
                // `vike-tradehub-core` calls it before the first start and the start hands the answer to
                // `with_halt_path`, which is the same moment and the same report (decision 0099).
                Ok(Started { session: SidecarProcess { child, reader }, account, amend_supported })
            }
            other => {
                let _ = child.kill();
                let _ = child.wait();
                *lock(&shared.stdin) = None;
                let _ = reader.join();
                Err(match other {
                    Ok(Err((reason, kind))) => {
                        AttemptFailed::Refused(format!("the sidecar said fatal: {reason}"), kind)
                    }
                    Err(std_mpsc::RecvTimeoutError::Timeout) => AttemptFailed::NoReady,
                    _ => AttemptFailed::Refused(
                        "the sidecar exited before it said ready".to_string(),
                        ErrorKind::Unknown,
                    ),
                })
            }
        }
    }

    /// **The account identifier this session's READY handshake carried**, verbatim — the venue's own
    /// answer to *which account did these credentials authenticate as*.
    ///
    /// This is the fact `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4.5 calls the
    /// handshake writer's input and §9 calls the evidence that dukascopy's book is not unknowable.
    /// A caller records it; NOTHING here writes a store — see [`Self::handshake_account`]'s field
    /// doc for why it is not a secret.
    ///
    /// ⚠ **Its FORM is not settled and this accessor asserts none.** The sidecar sends
    /// `IAccount.getAccountId()`, the one frame this crate pins is login-shaped (`"DEMO2cGyrc"`),
    /// and the numbers an operator reads off Dukascopy's own page are numeric. §9 records that as
    /// an open question to be settled by MEASUREMENT on an SDK-staged box, so a consumer that
    /// compares this against a stored value must treat a difference as a report rather than as
    /// proof — `vike_secrets::account_confirmation::verdict` is where that disposition lives.
    pub fn handshake_account(&self) -> &str {
        &self.handshake_account
    }

    /// Hand this client the sentinel it watches: `crate::adapter`'s `start_with` passes
    /// `AdapterInputs::process.halt_path`, the one file `vike-tradehub-core` resolved for the whole process,
    /// and a test passes a path of its own. Mirrors
    /// `vike_bridge_core::exec_actor::ExecutionClientActor::with_halt_path`; without it the client watches
    /// nothing (see the field's doc).
    pub fn with_halt_path(mut self, path: PathBuf) -> Self {
        self.halt_path = Some(path);
        self
    }

    /// True while the sentinel this client watches exists ⇒ refuse a submit that OPENS risk.
    ///
    /// Consulted at the submit boundary only, never on the reader thread's per-message path.
    fn halt_engaged(&self) -> bool {
        vike_execution::halt::sentinel_engaged(self.halt_path.as_deref())
    }

    /// Build a [`ReconcileClientDukascopy`] over this client's sidecar (the recon-breadth seam). Unlike the REST venues'
    /// `recon_client(config, symbol)` factories, the Dukascopy reconcile client is DERIVED from the running exec
    /// client: there is no REST endpoint to connect to, so every fetch is a `recon` question put to the sidecar this
    /// client owns (`crate::reconcile::query`). See `reconcile/mod.rs` for what each report is built from.
    pub fn recon_client(&self) -> ReconcileClientDukascopy {
        ReconcileClientDukascopy::new(Arc::new(ReconcileHandle {
            shared: Arc::clone(&self.shared),
        }))
    }

    /// **Lend this client's sidecar to an operator's read of `account`** (`crate::reconcile::venue_read`), which then starts no
    /// sidecar of its own. Weakly: a stopped client's sidecar is not kept alive by the loan, and the next start of the
    /// account lends its own.
    pub(crate) fn lend_to_reads(&self, account: &AccountLabel) {
        lock(&*LENT).insert(account.clone(), Arc::downgrade(&self.shared));
    }

    /// Whether the sidecar that runs now announced the reconcile query (`recon` in its `ready` line). The start hands the
    /// node a reconcile client only for a jar that did (`crate::adapter`'s `start_with`).
    pub fn reconcile_supported(&self) -> bool {
        self.shared.reconcile.supported()
    }

    /// Change how long a reconcile question waits for its answer (a test's seam; the production bound is
    /// `crate::reconcile::query::RECONCILE_QUERY_TIMEOUT`).
    pub fn set_reconcile_timeout(&self, timeout: Duration) {
        self.shared.reconcile.set_timeout(timeout);
    }

    /// Where the sidecar is now: `VenueLinkState::Up`, or `VenueLinkState::Down` with the sidecar's own words, how
    /// many restarts have failed and how long until the next (`sidecar_down`). The same answer that every refusal
    /// carries.
    pub fn sidecar_state(&self) -> VenueLinkState {
        self.shared.state()
    }

    /// The words of a refusal while no sidecar runs; `None` while one does.
    fn down_words(&self) -> Option<String> {
        down_words(&self.shared.state())
    }

    /// Whether `order_client_id` was in flight when a sidecar died: the sidecar that replaced it has no memory of the order.
    fn stranded(&self, order_client_id: &str) -> bool {
        lock(&self.shared.stranded).contains(order_client_id)
    }

    /// Write one command line; `false` marks the bridge dead (stdin dropped).
    fn write_line(&mut self, line: &str) -> bool {
        self.shared.stdin.send(line)
    }

    /// Tell the sidecar to exit, and WAIT FOR NOTHING — the first half of [`Self::shutdown_child`],
    /// hoisted to back `ExecutionClient::begin_detach`
    /// (`crates/vike-execution/src/execution_engine/client.rs`). On this venue the wind-down being
    /// overlapped is a CHILD PROCESS's exit rather than a socket read timeout, but the shape is the
    /// same: `shutdown_child` then spends up to `SHUTDOWN_GRACE` polling `try_wait`, and a core
    /// tearing down several venues should have started that clock on all of them first.
    ///
    /// ⚠ **It stops the supervisor FIRST**: the sidecar's exit that this very command causes must not be read as a
    /// death and restarted. Idempotent: the write is `let _ =` and clearing an already-cleared `stdin` is a no-op, so
    /// [`Self::shutdown_child`] running after this re-signals harmlessly — its own
    /// `write_line` simply fails into the same ignored result.
    fn signal_child_shutdown(&mut self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        let _ = self.wake_supervisor.send(());
        let _ = self.write_line(&encode_line(&SidecarCommand::Shutdown));
        // close the pipe: stdin EOF is the sidecar's fallback exit signal (and the price lane's writes now fail)
        *lock(&self.shared.stdin) = None;
    }

    /// `shutdown` → ≤5s grace → kill → reap child + reader (idempotent). The signalling half is
    /// [`Self::signal_child_shutdown`]; everything below it WAITS.
    fn shutdown_child(&mut self) {
        self.signal_child_shutdown();
        // `stopping` is set, so the supervisor installs no sidecar from here on (it kills one it had in hand): what
        // is in the slot now is the last.
        let session = lock(&self.shared.session).take();
        if let Some(SidecarProcess { mut child, reader }) = session {
            let deadline = Instant::now() + SHUTDOWN_GRACE;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
            // Bounded: the reader normally exits at pipe EOF, but it can be parked in
            // blocking_send on a stalled ingest channel — killing the child doesn't
            // unblock that. Wait briefly, then detach; the thread dies on its own when
            // the channel unblocks/closes (bounded teardown beats a guaranteed join).
            let _ = join_within(reader, SHUTDOWN_GRACE);
        }
        // The supervisor wakes on the stop and ends within one poll of a login in progress.
        if let Some(handle) = self.supervisor.take() {
            let _ = join_within(handle, SHUTDOWN_GRACE);
        }
    }
}

/// The running clients' sidecars by account, lent to an operator's read ([`ExecutionClientDukascopy::lend_to_reads`]).
static LENT: LazyLock<Mutex<HashMap<AccountLabel, Weak<Shared>>>> = LazyLock::new(Default::default);

/// The reconcile question of `account`'s running client, as its reconcile client asks it; `None` when no client of the
/// account runs in this process.
pub(crate) fn lent_reconcile_source(account: &AccountLabel) -> Option<Arc<dyn ReconcileSource>> {
    let shared = lock(&*LENT).get(account)?.upgrade()?;
    Some(Arc::new(ReconcileHandle { shared }))
}

/// What the reconcile client reads from the running client ([`ReconcileSource`]): the question (refused by name while no
/// sidecar runs), the protections the node holds resting, and the symbols that get a flat row.
struct ReconcileHandle {
    shared: Arc<Shared>,
}

impl ReconcileSource for ReconcileHandle {
    fn ask(&self) -> Result<ReconcileAnswer, String> {
        // A sidecar that is down answers nothing and the supervisor is starting another: say so, with the reason.
        if let Some(words) = down_words(&self.shared.state()) {
            return Err(
                crate::refusals::DukascopyReconcileRefusal::SidecarDown { words }.to_string()
            );
        }
        // The sidecar names each row by its wire name; the reports compare with the node's books under user names.
        self.shared
            .reconcile
            .ask()
            .map(crate::reconcile::in_user_names)
            .map_err(|refusal| refusal.to_string())
    }

    /// The protections the sidecar ACKNOWLEDGED and the node still holds (`Shared::protections` is kept from the submit to
    /// the order's terminal event, so an unanswered one is not yet resting). The two locks are taken one after the other,
    /// never nested: the reader takes them the same way.
    fn protections(&self) -> Vec<(String, OrderRequest)> {
        let resting: HashSet<String> = lock(&self.shared.underway)
            .iter()
            .filter(|(_, u)| u.acknowledged && u.is_protection())
            .map(|(order_client_id, _)| order_client_id.clone())
            .collect();
        lock(&self.shared.protections)
            .iter()
            .filter(|(order_client_id, _)| resting.contains(*order_client_id))
            .map(|(order_client_id, request)| (order_client_id.clone(), request.clone()))
            .collect()
    }

    fn know(&self, symbol: &str) {
        self.shared.reconcile.know(symbol);
    }

    fn forget(&self, symbol: &str) {
        self.shared.reconcile.forget(symbol);
    }

    fn known_symbols(&self) -> Vec<String> {
        self.shared.reconcile.known_symbols()
    }
}

/// The supervisor thread's body: park while a sidecar lives (saying its `link gap summary` once a minute and watching
/// its silence, [`watch_liveness`]), and when it ends, or is killed as wedged, start another until the client stops.
///
/// Never gives up while the client lives (the owner's order: a dead venue path comes back by itself, no hand step),
/// and never spins: every restart waits the next step of the one reconnect ladder over the row
/// (`vike_venues::reconnect::ReconnectBackoff`), and a start is bounded by
/// `Launch::attempt_timeout`.
/// While the FX market is closed on the config's clock (`DukascopyConfig::fx_clock`) no sidecar is started: the wait runs
/// to the open (`vike_venues::session::fx_login_held_pause`).
fn supervise(
    launch: &Launch,
    shared: &Arc<Shared>,
    wake: &std_mpsc::Sender<()>,
    parked: &std_mpsc::Receiver<()>,
) {
    // The restarts that failed, and ALSO a restarted sidecar that died before it had lived `restart.max` (a flap:
    // `ready`, then cut off again), which a count started over at every death would answer with the first, shortest
    // wait for ever: a login every ~5 s against a venue that keeps ending the session. Only a sidecar that lived that
    // long starts the count and the ladder over. The FIRST sidecar's death is always a fresh outage. The ladder takes
    // one step per restart, so it stands at step `failed` whenever a restart waits.
    let mut failed = 0u32;
    // Unspread: this restarts the account's OWN local sidecar process, and its `Down` state publishes the exact wait
    // (`retry_in`) the operator reads and the tests pin.
    let mut ladder = ReconnectBackoff::from_row(&shared.restart).without_jitter();
    let mut back_at: Option<Instant> = None;
    let look_every =
        (launch.liveness.probe_after / LIVENESS_LOOKS_PER_PROBE).min(GAP_SUMMARY_EVERY);
    let mut probed_at: Option<Instant> = None;
    loop {
        // Parked: a sidecar's reader says it ended (`ended.send`), the client says stop (`wake_supervisor`). Meanwhile,
        // every `look_every`, a look at the living sidecar's silence and, once a minute, its gap summary
        // (`vike_bridge_core::link_gap`): a line, and back to the park.
        match parked.recv_timeout(look_every) {
            Ok(()) => {}
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                let due = lock(&shared.gaps).take_due(now);
                if let Some(summary) = due {
                    summary.say("dukascopy");
                }
                watch_liveness(&launch.liveness, shared, &mut probed_at, now);
                continue;
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if shared.is_stopping() {
            return;
        }
        failed = match back_at {
            Some(at) if at.elapsed() < shared.restart.max => failed + 1,
            _ => {
                ladder.reset();
                0
            }
        };
        // The dead sidecar's reader has finished its drain; reap the child so no second JForex session can ever
        // share the login with it, then start over.
        let dead = lock(&shared.session).take();
        if let Some(dead) = dead {
            dead.kill_and_reap();
        }
        *lock(&shared.stdin) = None;
        let down_since = Instant::now();
        let died_of = match shared.state() {
            VenueLinkState::Down(VenueLinkDown { why, .. }) => why,
            VenueLinkState::Up | VenueLinkState::Unknown => "its output ended".to_string(),
        };
        // The latest answer to "why is it down": the death's, then each failed restart's.
        let mut why_now = died_of.clone();
        loop {
            // A sidecar is a JForex login, and none is made while the FX market is closed: a restart that would fall
            // inside it waits ONCE for the open (owner ruling 2026-11-13).
            let wait = ladder.failed();
            let delay = launch.config.fx_clock.as_ref().map_or(wait, |clock| {
                vike_venues::session::fx_login_held_pause(
                    crate::reconcile::VENUE,
                    wait,
                    clock.now_ms(),
                )
            });
            shared.set_state(sidecar_down(why_now.clone(), failed, delay));
            tracing::warn!(
                target: TARGET,
                failed_restarts = failed,
                retry_in = ?delay,
                why = %why_now,
                "the JForex sidecar is still DOWN: starting another"
            );
            if !pause(shared, parked, delay) {
                return;
            }
            match ExecutionClientDukascopy::start_sidecar(
                launch,
                shared,
                wake,
                launch.attempt_timeout,
            ) {
                Ok(started) => {
                    let mut slot = lock(&shared.session);
                    // Installed under the slot's lock, and `stopping` is read under it: a detach that ran
                    // first took the slot empty and set the flag, so this sidecar is killed here and a detach
                    // that runs later finds it in the slot. Either way none outlives the client.
                    if shared.is_stopping() {
                        drop(slot);
                        started.session.kill_and_reap();
                        *lock(&shared.stdin) = None;
                        return;
                    }
                    shared.amend_supported.store(started.amend_supported, Ordering::SeqCst);
                    *slot = Some(started.session);
                    drop(slot);
                    back_at = Some(Instant::now());
                    shared.set_state(VenueLinkState::Up);
                    tracing::warn!(
                        target: TARGET,
                        account = %started.account,
                        failed_restarts = failed,
                        down_secs = down_since.elapsed().as_secs(),
                        "the JForex sidecar is BACK (bridge ready): orders are accepted again. ⚠ A jar that adopts \
                         (the `adopted` line above) has taken up the orders the venue lists and can cancel and amend \
                         them; one that does not has no memory of the orders the dead one placed: a cancel or amend \
                         of an order that was in flight at the death is refused by name, and a position that moved \
                         during the outage shows at the next fill's position line"
                    );
                    break;
                }
                Err(failure) => {
                    failed += 1;
                    let reason = match failure {
                        AttemptFailed::NoReady => {
                            "no ready within the start's time limit (killed it)".to_string()
                        }
                        AttemptFailed::Refused(why, _) => why,
                    };
                    tracing::warn!(
                        target: TARGET,
                        failed_restarts = failed,
                        %reason,
                        "restarting the JForex sidecar failed; it will be tried again"
                    );
                    why_now = format!("{died_of}; the last restart failed: {reason}");
                    if shared.is_stopping() {
                        return;
                    }
                }
            }
        }
    }
}

/// The parked supervisor's look at the living sidecar ([`Liveness`]). Nothing before its `ready`, nor for a jar without
/// `recon` (`Shared::heard` is `None`). Silent for `probe_after`: the `recon` probe, again every `probe_after` while it
/// stays silent. Silent for `idle_limit`: it is WEDGED and KILLED, with a warn naming the silence and the limit. Only
/// killed: its reader's EOF then says it DOWN in these words (`Shared::went_down`, which ends every order in flight as a
/// death does) and wakes this supervisor, whose restart path reaps it and starts the next one on the reconnect ladder, so
/// the next JForex session starts only after this one is reaped.
fn watch_liveness(
    liveness: &Liveness,
    shared: &Arc<Shared>,
    probed_at: &mut Option<Instant>,
    now: Instant,
) {
    let Some(heard) = *lock(&shared.heard) else { return };
    let silence = now.saturating_duration_since(heard);
    let limit = liveness.idle_limit;
    if silence >= limit {
        tracing::warn!(
            target: TARGET,
            silence = ?silence,
            idle_limit = ?limit,
            "the JForex sidecar wrote nothing for {silence:.1?}, past its idle limit of {limit:.1?} (it answered \
             no probe): it is treated as dead (a wedged JVM, strategy thread or stdio), killed, and started again \
             like a dead one"
        );
        *lock(&shared.last_fatal) = Some(format!(
            "it wrote nothing for {silence:.1?}, past its idle limit of {limit:.1?}, and was killed as wedged"
        ));
        *lock(&shared.heard) = None;
        if let Some(session) = lock(&shared.session).as_mut() {
            let _ = session.child.kill();
        }
        return;
    }
    let probe_due =
        probed_at.is_none_or(|at| now.saturating_duration_since(at) >= liveness.probe_after);
    if silence >= liveness.probe_after && probe_due {
        *probed_at = Some(now);
        // On a thread of its own: a sidecar that reads no stdin either blocks the write, and the kill above must never
        // wait on the sidecar. The kill closes the pipe, which ends a write that blocked.
        let shared = Arc::clone(shared);
        let _ = std::thread::Builder::new()
            .name("duka-probe".into())
            .spawn(move || shared.reconcile.probe());
    }
}

/// Wait `delay`, or less if the client stops; `false` when it did.
fn pause(shared: &Shared, parked: &std_mpsc::Receiver<()>, delay: Duration) -> bool {
    let until = Instant::now() + delay;
    loop {
        if shared.is_stopping() {
            return false;
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        // A stop wakes this at once; any other message is a stale wake that only re-checks the clock.
        let _ = parked.recv_timeout(left);
    }
}

impl ExecutionClient for ExecutionClientDukascopy {
    fn submit(&mut self, request: &OrderRequest) {
        // A reduce-only LIMIT is the native TAKE-PROFIT of the open orders, but only for a jar that says so
        // (`FEATURE_TAKE_PROFIT`). A jar from before it places such an order as a plain pending order, which OPENS an
        // opposite position when it is hit, so it is refused by name, terminally, before anything is sent. (The row
        // lists `limit` among the reduce-only kinds, so the core's preflight lets it, and a bracket's take-profit leg,
        // through to here.)
        if request.reduce_only
            && request.order_type == vike_model::OrderType::Limit
            && !self.shared.take_profit_supported.load(Ordering::SeqCst)
        {
            let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected { cause: None,
                order_client_id: request.order_client_id.clone(),
                reason: "dukascopy: a reduce-only LIMIT order is not wired on the installed JForex sidecar jar: it \
                         predates the native take-profit, so a resting limit would OPEN an opposite position when \
                         hit instead of closing the open one; install the jar of the release that adds it \
                         (`just fetch-tools`) and restart, or close with a reduce-only market order or protect with \
                         a reduce-only stop; this order was NOT sent"
                    .into(),
                timestamp: request.timestamp,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::VenueCapability.word(),
                ),
            }));
            return;
        }
        // A stop-limit or an if-touched order is the venue's own conditional ENTRY order, but only for a jar that says so
        // (`FEATURE_TRIGGER_ENTRY`; one from before it answers "unsupported order_type"), and never reduce-only: JForex
        // has no reduce-only form, and hit it would OPEN a position (the jar refuses it too; this is the refusal the
        // capability row's `reduce_only_kinds` is held to). Refused by name, terminally, before anything is sent.
        if matches!(
            request.order_type,
            vike_model::OrderType::StopLimit | vike_model::OrderType::TakeProfit
        ) {
            let kind = request.order_type.as_str();
            let refusal = if !self.shared.trigger_entry_supported.load(Ordering::SeqCst) {
                Some(format!(
                    "dukascopy: a {kind} order is not wired on the installed JForex sidecar jar: it predates the \
                     native stop-limit and if-touched orders (its `ready` does not announce `{FEATURE_TRIGGER_ENTRY}`); \
                     install a jar that does and restart the gatewayhub; this order was NOT sent"
                ))
            } else if request.reduce_only {
                Some(format!(
                    "dukascopy: a reduce-only {kind} order is refused: the venue holds it as an ENTRY order (JForex has \
                     no reduce-only form), which would OPEN a position when hit; protect the position with a reduce-only \
                     stop (the venue's stop-loss) or a reduce-only limit (its take-profit); this order was NOT sent"
                ))
            } else {
                None
            };
            if let Some(reason) = refusal {
                let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
                    cause: None,
                    order_client_id: request.order_client_id.clone(),
                    reason: reason.into(),
                    timestamp: request.timestamp,
                    refusal_id: vike_model::events::refusal_id_of(
                        vike_model::OrderRefusalId::VenueCapability.word(),
                    ),
                }));
                return;
            }
        }
        // HALT kill-switch. While the sentinel exists, refuse a submit that OPENS or ADDS risk and
        // synthesize the terminal rejection — the same shape `ExecutionClientActor` uses, checked BEFORE the
        // `OrderSubmitted` half so a refused order never enters the sidecar's inflight set.
        //
        // ⚠ A REDUCING submit is ADMITTED (`vike_execution::halt::halt_admits_submit`, the ONE
        // definition of that rule): a halt must never trap the operator in a position, and this
        // venue holds no position book at this boundary, so the caller-asserted flag is the only
        // evidence there is.
        //
        // This client is BESPOKE — it owns a child process rather than an `ExecutionClientActor` — so it
        // inherited nothing, and for a long time it consulted the sentinel nowhere.
        // `docs/ops/kill-switches.md` carried that as a LATENT gap rather than a live one, because
        // nothing in production started this venue: the risk was somebody wiring the venue without
        // wiring the switch. Closing it here means the venue can be wired by itself.
        if self.halt_engaged() && !vike_execution::halt::halt_admits_submit(request) {
            let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: request.order_client_id.clone(),
                reason: vike_execution::halt::HALT_REJECT_REASON.into(),
                timestamp: request.timestamp,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::HaltFile.word(),
                ),
            }));
            return;
        }
        // No sidecar runs (it died and the supervisor is starting another): refuse by name, terminally, before the
        // order enters the in-flight set. Nothing was sent, so nothing can have reached the venue.
        if let Some(words) = self.down_words() {
            let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: request.order_client_id.clone(),
                reason: format!("{BRIDGE_UNAVAILABLE}: {words}; this order was NOT sent").into(),
                timestamp: request.timestamp,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::NotSent.word(),
                ),
            }));
            return;
        }
        // The engine names the instrument by its USER name; the sidecar is sent its kept row's WIRE name (decision
        // 0290). No list, no row or two rows: refused by name, terminally, before anything is sent.
        let sent = match to_the_wire(request) {
            Ok(sent) => sent,
            Err(why) => {
                let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
                    cause: None,
                    order_client_id: request.order_client_id.clone(),
                    reason: format!("{why}; this order was NOT sent").into(),
                    timestamp: request.timestamp,
                    refusal_id: vike_model::events::refusal_id_of(
                        vike_model::OrderRefusalId::NotSent.word(),
                    ),
                }));
                return;
            }
        };
        // Rust-side synchronous half of the lifecycle.
        let _ = self.events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
            order_client_id: request.order_client_id.clone(),
            timestamp: request.timestamp,
        }));
        // Track BEFORE writing: if the child dies with this order in flight, the reader's EOF drain answers it (the
        // cancel of an order the sidecar had taken up, `Underway::cancel_words_at_death`; nothing yet for one it never
        // answered, whose outcome is unknown: `settle_unanswered`).
        let underway = Underway::of(request);
        if underway.is_protection() {
            // Kept to send again to the next sidecar if this one dies with it resting (`hand_back_protections`).
            lock(&self.shared.protections).insert(request.order_client_id.clone(), request.clone());
        }
        lock(&self.shared.underway).insert(request.order_client_id.clone(), underway);
        let line = encode_line(&SidecarCommand::Submit { order: Box::new(sent) });
        if !self.write_line(&line) {
            // Dead bridge: the order must still reach a terminal state (spec). Only
            // reject if the reader's EOF drain (or a real terminal event) hasn't
            // already taken this order client id — belt and braces without double-rejecting.
            lock(&self.shared.protections).remove(&request.order_client_id);
            let still_inflight =
                lock(&self.shared.underway).remove(&request.order_client_id).is_some();
            if still_inflight {
                let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected { cause: None,
                    order_client_id: request.order_client_id.clone(),
                    reason: format!(
                        "{BRIDGE_UNAVAILABLE}: the write to the JForex sidecar failed (it has exited and the \
                         node is restarting it); this order was NOT sent"
                    )
                    .into(),
                    timestamp: request.timestamp,
                    refusal_id: vike_model::events::refusal_id_of(
                        vike_model::OrderRefusalId::NotSent.word(),
                    ),
                }));
            }
        }
    }

    /// A cancel is never gated by a halt. While no sidecar runs, or when the order was in flight at a sidecar's
    /// death (the new one does not know it), it is refused by name with the non-terminal `OrderCancelRejected`
    /// instead of being dropped in silence (the sidecar's own `handleCancel` skips an unknown order without a word).
    fn cancel(&mut self, order_client_id: &str) {
        let refuse = |events: &EventSender, reason: String| {
            let _ = events.blocking_send(Event::OrderCancelRejected(OrderCancelRejected {
                order_client_id: order_client_id.to_string(),
                reason: reason.into(),
                timestamp: vike_model::now_ms(),
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::NotSent.word(),
                ),
            }));
        };
        if let Some(words) = self.down_words() {
            refuse(
                &self.events,
                format!("dukascopy: {BRIDGE_UNAVAILABLE}: {words}; the cancel was NOT sent"),
            );
            return;
        }
        if self.stranded(order_client_id) {
            refuse(&self.events, stranded_words("cancel"));
            return;
        }
        // Marked BEFORE the write: a death right after it must not send the protection to the next sidecar blind.
        if lock(&self.shared.protections).contains_key(order_client_id) {
            lock(&self.shared.cancelling).insert(order_client_id.to_string());
        }
        let line = encode_line(&SidecarCommand::Cancel { order_client_id: order_client_id.into() });
        if !self.write_line(&line) {
            lock(&self.shared.cancelling).remove(order_client_id);
            tracing::warn!(target: TARGET, order_client_id, "cancel not sent — the sidecar's pipe is closed");
            refuse(
                &self.events,
                format!(
                    "dukascopy: {BRIDGE_UNAVAILABLE}: the write to the JForex sidecar failed (it has exited \
                     and the node is restarting it); the cancel was NOT sent"
                ),
            );
        }
    }

    /// Amend a resting limit order in place (`IOrder.setOpenPrice` / `setRequestedAmount` in the sidecar). The
    /// sidecar answers with `OrderModified` (the terms JForex now holds) or `OrderModifyRejected`, both through
    /// the reader, or, when JForex answered neither way in time, an unsettled line ([`answer_unsettled`]); this side only
    /// refuses what it can know without asking.
    ///
    /// ⚠ **HALT blocks a modify outright**, like every client that has a sentinel (`ExecutionClientActor::modify`, ctrader):
    /// an amend can raise the size of a resting order, and the exit path under a halt is a cancel. It says so with
    /// the non-terminal advisory instead of returning in silence.
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        self.send_modify(order, new_qty, new_price, None);
    }

    /// Move a resting stop-limit's or if-touched order's TRIGGER (`StrategyBridge.java`'s `handleModify` with a
    /// `new_trigger_price`: `setOpenPrice(trigger, slippage)`). The SDK states no slippage back, so the sidecar is sent
    /// the limit to KEEP beside the trigger and recomputes the slippage from the two: the amend's own limit, else the
    /// one the order rests with. A market-if-touched order (a `take_profit` with no limit) moves its trigger alone. What
    /// this side knows it cannot send is refused in words (the engine publishes them as `OrderModifyRejected`); the
    /// sidecar refuses the rest, a limit the new trigger would leave on the better side among them.
    fn modify_trigger(
        &mut self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: f64,
    ) -> Result<(), &'static str> {
        use vike_model::OrderType::{StopLimit, TakeProfit};
        if !matches!(order.order_type, StopLimit | TakeProfit) {
            return Err(TRIGGER_AMEND_NO_ENTRY);
        }
        if !self.shared.trigger_entry_supported.load(Ordering::SeqCst) {
            return Err(TRIGGER_AMEND_OLD_JAR);
        }
        let limit = match (&order.order_type, order.price) {
            // market-if-touched: no limit, and none may be added (its comment names it market-if-touched)
            (TakeProfit, None) if new_price.is_some() => {
                return Err(TRIGGER_AMEND_MIT_HAS_NO_LIMIT);
            }
            (TakeProfit, None) => None,
            (_, rests_with) => Some(new_price.or(rests_with).ok_or(TRIGGER_AMEND_LIMIT_UNKNOWN)?),
        };
        self.send_modify(order, new_qty, limit, Some(new_trigger_price));
        Ok(())
    }

    /// Phase-one teardown seam — see `crates/vike-execution/src/execution_engine/client.rs`'s
    /// `ExecutionClient::begin_detach`. This venue owns a CHILD PROCESS rather than a socket pump,
    /// so what gets overlapped here is the sidecar's own exit.
    fn begin_detach(&mut self) {
        self.signal_child_shutdown();
    }
    fn detach(&mut self) {
        self.shutdown_child();
    }
}

/// [`ExecutionClient::modify_trigger`]'s refusal of a kind that has no conditional entry here (the engine sends only
/// a stop-limit or a take-profit; a stop's trigger rides `modify`'s price).
const TRIGGER_AMEND_NO_ENTRY: &str = "only a stop-limit or an if-touched (take_profit) order has a trigger this \
     adapter moves in place; nothing was sent and the order keeps its terms";

/// [`ExecutionClient::modify_trigger`]'s refusal on a jar that does not announce [`FEATURE_TRIGGER_ENTRY`].
const TRIGGER_AMEND_OLD_JAR: &str = "the installed JForex sidecar jar predates the native stop-limit and \
     if-touched orders (`trigger-entry`), so it cannot move a trigger; nothing was sent and the order keeps its terms";

/// [`ExecutionClient::modify_trigger`]'s refusal of a limit added to a market-if-touched order.
const TRIGGER_AMEND_MIT_HAS_NO_LIMIT: &str = "a market-if-touched order has no limit price to set (the venue \
     holds it as one, by its comment): move its trigger alone, or cancel it and place a limit-if-touched order; \
     nothing was sent";

/// [`ExecutionClient::modify_trigger`]'s refusal when no limit is known to keep (a stop-limit restored with none).
const TRIGGER_AMEND_LIMIT_UNKNOWN: &str = "the order's limit price is not known here, and the venue states \
     none back (only a slippage from the trigger, unreadable), so the trigger cannot be moved with the limit kept; \
     name the limit in the amend; nothing was sent";

impl ExecutionClientDukascopy {
    /// The `modify` command for an amend of qty, limit and (`new_trigger_price`) trigger, after the refusals this side
    /// can make without asking, each a non-terminal `OrderModifyRejected` in words. `modify` sends no trigger, so its
    /// line is the bytes it always was.
    fn send_modify(
        &mut self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: Option<f64>,
    ) {
        use vike_model::OrderRefusalId::{HaltFile, NotSent, VenueCapability};
        let refuse = |events: &EventSender, reason: &str, refusal: vike_model::OrderRefusalId| {
            let _ = events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
                order_client_id: order.order_client_id.clone(),
                reason: reason.into(),
                timestamp: vike_bridge_core::rest::amend_answer_time(None),
                refusal_id: vike_model::events::refusal_id_of(refusal.word()),
            }));
        };
        if self.halt_engaged() {
            refuse(&self.events, vike_execution::halt::HALT_REJECT_REASON, HaltFile);
            return;
        }
        if new_qty.is_none() && new_price.is_none() && new_trigger_price.is_none() {
            refuse(
                &self.events,
                "dukascopy: an amend names neither a new quantity nor a new price",
                NotSent,
            );
            return;
        }
        if let Some(words) = self.down_words() {
            refuse(
                &self.events,
                &format!("dukascopy: {BRIDGE_UNAVAILABLE}: {words}; the amend was NOT sent"),
                NotSent,
            );
            return;
        }
        if self.stranded(&order.order_client_id) {
            refuse(&self.events, &stranded_words("amend"), NotSent);
            return;
        }
        if !self.shared.amend_supported.load(Ordering::SeqCst) {
            refuse(
                &self.events,
                "dukascopy: the installed JForex sidecar jar predates the amend command; install the \
                 jar from this release (`just fetch-tools`) and restart",
                VenueCapability,
            );
            return;
        }
        let line = encode_line(&SidecarCommand::Modify {
            order_client_id: order.order_client_id.clone(),
            new_qty,
            new_price,
            new_trigger_price,
            timestamp: order.timestamp,
        });
        if !self.write_line(&line) {
            // The order is still live (or gone with the child): the amend is refused, non-terminal; the
            // reader's EOF drain owns the order's own terminal.
            refuse(
                &self.events,
                &format!(
                    "dukascopy: {BRIDGE_UNAVAILABLE}: the write to the JForex sidecar failed (it has \
                     exited and the node is restarting it); the amend was NOT sent"
                ),
                NotSent,
            );
        }
    }
}

impl Drop for ExecutionClientDukascopy {
    fn drop(&mut self) {
        self.shutdown_child(); // idempotent: try_wait returns Ok(Some) once reaped
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// The READY handshake's bound is the exec row's 300 s, the figure the sidecar's own login
    /// polling (up to 240 s) is sized under, and the Java sidecar's `JnlpRetry` arithmetic cites it
    /// by name, so it is pinned here as a literal rather than as a restatement of the row.
    #[test]
    fn the_ready_timeout_is_the_execution_rows_three_hundred_seconds() {
        assert_eq!(READY_TIMEOUT, Duration::from_secs(300));
    }

    /// **The sidecar is sent the kept row's WIRE name of an order's USER name** (decision 0290): `AAPL.CFD` goes out as
    /// `AAPL.US/USD`, a pair and a metal as their six letters, and only the symbol changes. A name the kept list carries no
    /// row of (an old spelling, the enum-style name of a CFD) is refused in words and nothing is guessed. The way back
    /// is `crates/bridges/dukascopy/tests/sidecar/dukascopy_exec.rs`'s `an_order_by_its_user_name_is_filled_under_its_user_name`.
    #[test]
    fn the_sidecar_is_sent_the_kept_rows_wire_name() {
        crate::test_support::hold_kept_list_for_tests();
        let named = |symbol: &str| OrderRequest {
            symbol: symbol.to_string(),
            ..crate::test_support::order("c")
        };
        for (user, wire) in [
            ("EURUSD", "EURUSD"),
            ("eurusd", "EURUSD"),
            ("XAUUSD", "XAUUSD"),
            ("AAPL.CFD", "AAPL.US/USD"),
        ] {
            let sent = to_the_wire(&named(user)).expect(user);
            assert_eq!(sent, OrderRequest { symbol: wire.to_string(), ..named(user) }, "{user}");
        }
        for old in ["AAPL.US/USD", "AAPLUSUSD", "EUR/USD", "NOPEUSD"] {
            let why = to_the_wire(&named(old)).expect_err(old);
            assert!(why.contains("dukascopy") && why.contains("carries no row"), "{old}: {why}");
        }
    }

    /// The live gate reports the path it actually TRIED, which is the whole operator-facing point
    /// of resolving in the composition root: `spawn` no longer has an opinion about where a jar
    /// lives, so a wrong answer has to be visible as a wrong PATH rather than as a venue that went
    /// quiet. (`vendored_java_picks_highest_jdk_or_none` stood here until the sweep moved to
    /// `config.rs` beside the ladder it belongs to; `the_jre_sweep_picks_the_highest_image_or_falls_through`
    /// is its successor.)
    #[test]
    fn a_missing_jar_is_unavailable_rather_than_a_spawn() {
        let (tx, _rx) = vike_execution::event_channel(4);
        let tools = DukascopyTools {
            java: "java".into(),
            bridge_jar: PathBuf::from("Z:/no-such-project/bin/jforex/jforex-bridge.jar"),
            jforex_home: None,
        };
        let config = DukascopyConfig {
            login: "l".into(),
            password: vike_model::secret::Secret::new("p".into()),
            server: String::new(),
            ledger_mount: None,
            fx_clock: None,
        };
        assert_eq!(
            ExecutionClientDukascopy::spawn(config, &tools, tx).err(),
            Some(DukascopyError::Unavailable(ErrorKind::Unknown)),
            "a missing jar is not the venue's failure: it parks"
        );
    }

    fn tools_with_home(home: Option<&str>) -> DukascopyTools {
        DukascopyTools {
            java: "java".into(),
            bridge_jar: PathBuf::from("/p/bin/jforex/jforex-bridge.jar"),
            jforex_home: home.map(PathBuf::from),
        }
    }

    /// ⚠ **This asserts the ARGUMENT VECTOR, not the BEHAVIOUR.** It proves what `spawn` would hand
    /// `std::process::Command` and nothing about what a JVM does with it — no process is started
    /// here and no `user.home` is observed. The behavioural half, which starts a real child and
    /// reads back the home that child was actually given, is
    /// `crates/bridges/dukascopy/tests/sidecar/dukascopy_exec.rs`'s
    /// `the_sidecar_is_started_with_the_projects_jforex_home`; even that one models the JVM rather
    /// than being one.
    ///
    /// What it DOES carry on its own is the ordering rule, which is a real correctness property:
    /// the flag is a JVM option, so `-jar` must come after it or the JVM never sees it.
    #[test]
    fn the_home_flag_is_a_jvm_option_and_precedes_the_jar() {
        let args = jvm_args(&tools_with_home(Some("/p/settings/state/jforex")));
        assert_eq!(
            args,
            vec![
                "-Duser.home=/p/settings/state/jforex".to_string(),
                "-Xmx512m".to_string(),
                "-jar".to_string(),
                "/p/bin/jforex/jforex-bridge.jar".to_string(),
            ]
        );
        let flag = args.iter().position(|a| a.starts_with(USER_HOME_FLAG)).expect("the flag");
        let jar = args.iter().position(|a| a == "-jar").expect("-jar");
        assert!(flag < jar, "a -D flag after -jar is an argument to Bridge.main, not a JVM option");
    }

    /// No home resolved → NO `-D` flag, and the rest of the command line is what this venue has
    /// always spawned plus the heap cap. The `None` rung must leave the JVM's own `user.home` alone
    /// rather than passing an empty or guessed one: `-Duser.home=` with nothing after it would point
    /// the SDK at the working directory, which is worse than the default it replaced.
    #[test]
    fn no_resolved_home_emits_no_home_flag_at_all() {
        let args = jvm_args(&tools_with_home(None));
        assert_eq!(
            args,
            vec![
                "-Xmx512m".to_string(),
                "-jar".to_string(),
                "/p/bin/jforex/jforex-bridge.jar".to_string()
            ]
        );
        assert!(args.iter().all(|a| !a.starts_with("-D")), "{args:?}");
    }

    /// **The heap cap is one literal argv element, in the JVM's option section, with or without a
    /// home** — the 512 MiB figure is the arithmetic in [`MAX_HEAP_FLAG`]'s doc (a quarter of the
    /// trading unit's `MemoryMax=2G`), so the literal is pinned here: a drift of the number is a
    /// decision, not an edit. Like the home test above, this asserts the ARGUMENT VECTOR and says
    /// nothing about what a JVM does with it; the vector is split the way a JVM splits it
    /// (everything before `-jar` is the JVM's own).
    #[test]
    fn the_heap_cap_is_one_explicit_option_before_the_jar() {
        for home in [None, Some("/p/settings/state/jforex")] {
            let args = jvm_args(&tools_with_home(home));
            let jar = args.iter().position(|a| a == "-jar").expect("-jar");
            let jvm_section = &args[..jar];
            assert_eq!(
                jvm_section.iter().filter(|a| a.starts_with("-Xmx")).collect::<Vec<_>>(),
                [&"-Xmx512m".to_string()],
                "exactly one maximum-heap option, 512 MiB, before -jar: {args:?}"
            );
            assert!(
                args[jar + 1..].iter().all(|a| !a.starts_with("-X")),
                "a -X option after -jar is an argument to Bridge.main, not the JVM: {args:?}"
            );
        }
    }

    /// **A path with spaces or a drive letter is ONE argv element**, carried verbatim with nothing
    /// quoted, escaped or split by us — `std::process::Command` hands each element to the OS as a
    /// unit. The resolver half of the same Windows concern is
    /// `crates/bridges/dukascopy/src/config_tests.rs`'s
    /// `a_state_path_with_spaces_reaches_the_home_unaltered`.
    #[test]
    fn the_home_flag_is_one_argv_element_whatever_the_path() {
        let home = r"C:\Program Files\vike trader\settings\state\jforex";
        let args = jvm_args(&tools_with_home(Some(home)));
        let flag = &args[0];
        assert_eq!(flag, &format!("{USER_HOME_FLAG}{home}"));
        assert!(!flag.contains('"'), "we must not quote: {flag}");
        assert_eq!(
            flag.strip_prefix(USER_HOME_FLAG),
            Some(home),
            "the path must round-trip through the flag unaltered"
        );
    }

    /// **The restart wait is the `DUKASCOPY` row's: 5 s, doubling, never above a minute** (decision 0160: the numbers
    /// live in the row and `RESTART` reads them, so this pins the READING, and a retune of the row is the one edit
    /// that moves it). Never a tight loop, never a wait that grows without bound.
    #[test]
    fn the_restart_wait_doubles_from_five_seconds_to_a_minute_and_stays_there() {
        assert_eq!(RESTART.initial, Duration::from_secs(5));
        assert_eq!(RESTART.max, Duration::from_secs(60));
        let mut ladder = ReconnectBackoff::from_row(&RESTART).without_jitter();
        let waits: Vec<u64> = (0..8).map(|_| ladder.failed().as_secs()).collect();
        assert_eq!(waits, [5, 10, 20, 40, 60, 60, 60, 60]);
        let far: Vec<_> = (0..1_000).map(|_| ladder.failed()).collect();
        assert!(far.iter().all(|wait| *wait == RESTART.max), "no overflow, no escape");
    }

    fn market(
        order_client_id: &str,
        reduce_only: bool,
        order_type: vike_model::OrderType,
    ) -> OrderRequest {
        OrderRequest {
            order_client_id: order_client_id.into(),
            venue: "dukascopy".into(),
            symbol: "EURUSD".into(),
            side: -1,
            qty: 1000.0,
            order_type,
            reduce_only,
            ..Default::default()
        }
    }

    /// **Which orders get a cancel at a death is a SCOPE, and this pins it**: once the sidecar has acknowledged it, a
    /// stop-loss, a take-profit (a reduce-only limit), a reduce-only MARKET close and a plain MARKET entry (the orders
    /// whose venue state the node must not keep believing in); a plain resting limit keeps the rejection it always had,
    /// and so does every order the sidecar never answered. A stop-loss and a take-profit are the protections a sidecar
    /// that adopts keeps (`Underway::is_protection`); the close and the entry never are.
    #[test]
    fn an_acknowledged_protection_close_or_market_entry_gets_a_cancel_at_a_death_and_a_limit_does_not()
     {
        use vike_model::OrderType::{Limit, Market, Stop};
        let words = |o: &OrderRequest| {
            let mut u = Underway::of(o);
            assert_eq!(u.cancel_words_at_death(), None, "none starts acknowledged: {:?}", u.kind);
            u.acknowledged = true;
            u.cancel_words_at_death()
        };
        assert_eq!(words(&market("s", true, Stop)), Some(STOP_BRIDGE_DIED));
        assert_eq!(words(&market("s", false, Stop)), Some(STOP_BRIDGE_DIED));
        assert_eq!(words(&market("c", true, Market)), Some(CLOSE_BRIDGE_DIED));
        assert_eq!(words(&market("e", false, Market)), Some(ENTRY_BRIDGE_DIED));
        assert_eq!(words(&market("l", false, Limit)), None, "a resting limit keeps the rejection");
        assert_eq!(
            words(&market("r", true, Limit)),
            Some(TAKE_PROFIT_BRIDGE_DIED),
            "a reduce-only limit is the native take-profit: it rests at the venue like the stop-loss"
        );
        for (request, protection) in [
            (market("s", true, Stop), true),
            (market("r", true, Limit), true),
            (market("c", true, Market), false),
            (market("e", false, Market), false),
            (market("l", false, Limit), false),
        ] {
            assert_eq!(Underway::of(&request).is_protection(), protection, "{request:?}");
        }
    }

    fn fill(order_client_id: &str, last_qty: f64) -> vike_model::events::FillEvent {
        vike_model::events::FillEvent {
            trade_id: "t:1".into(),
            order_client_id: order_client_id.into(),
            venue: "dukascopy".into(),
            symbol: "EURUSD".into(),
            side: -1,
            last_qty,
            last_price: 1.1,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: vike_model::events::LiquiditySide::Taker,
            timestamp: 1,
            mark_price: None,
            position_side: vike_model::events::PositionSide::Both,
        }
    }

    fn names(settled: &(Event, Option<Event>)) -> Vec<&'static str> {
        std::iter::once(&settled.0)
            .chain(settled.1.as_ref())
            .map(|e| match e {
                Event::OrderAccepted(_) => "OrderAccepted",
                Event::OrderPartiallyFilled(_) => "OrderPartiallyFilled",
                Event::OrderFilled(_) => "OrderFilled",
                Event::OrderCanceled(_) => "OrderCanceled",
                Event::OrderRejected(_) => "OrderRejected",
                _ => "other",
            })
            .collect()
    }

    /// `settle`: an `OrderAccepted` or a partial fill acknowledges an order (and nothing else does); a rejection of an
    /// acknowledged order is a cancel with the venue's words, of an unanswered one a rejection still; a reduce-only
    /// close filled short ends partly filled then canceled in words, a full one filled, and an ENTRY is never relabelled.
    #[test]
    fn settle_folds_a_late_rejection_as_a_cancel_and_ends_a_short_close_partly_filled() {
        use vike_model::OrderType::{Limit, Market};
        use vike_model::events::{OrderAccepted, OrderFilled};
        let mut underway = HashMap::new();
        for (order_client_id, reduce_only, kind) in [
            ("late", false, Limit),
            ("early", false, Limit),
            ("close", true, Market),
            ("entry", false, Market),
        ] {
            let req = OrderRequest { qty: 2000.0, ..market(order_client_id, reduce_only, kind) };
            underway.insert(order_client_id.to_string(), Underway::of(&req));
        }
        let rejected = |order_client_id: &str| {
            Event::OrderRejected(OrderRejected {
                order_client_id: order_client_id.into(),
                reason: "fill rejected: no liquidity".into(),
                timestamp: 7,
                cause: None,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::VenueRefused.word(),
                ),
            })
        };
        let accepted = |order_client_id: &str| {
            Event::OrderAccepted(OrderAccepted {
                order_client_id: order_client_id.into(),
                venue_order_id: None,
                timestamp: 1,
            })
        };
        let filled = |order_client_id: &str, q| {
            Event::OrderFilled(OrderFilled {
                order_client_id: order_client_id.into(),
                fill: fill(order_client_id, q),
                timestamp: 1,
            })
        };

        // A rejection before any acknowledgement is the order's rejection, as it always was.
        assert_eq!(names(&settle(&mut underway, rejected("early"))), ["OrderRejected"]);
        // After the venue accepted it, the same rejection is a cancel carrying the venue's words.
        settle(&mut underway, accepted("late"));
        assert!(underway["late"].acknowledged);
        match settle(&mut underway, rejected("late")) {
            (Event::OrderCanceled(c), None) => {
                assert_eq!((c.order_client_id.as_str(), c.timestamp), ("late", 7));
                assert!(c.reason.contains("fill rejected: no liquidity"), "{}", c.reason);
                assert_eq!(
                    c.refusal_id.as_deref(),
                    Some("order.venue_refused"),
                    "the rejection's id"
                );
            }
            other => panic!("a late rejection must fold as a cancel, got {other:?}"),
        }

        // A reduce-only close of 2000: 500 then 500, and the venue held no more.
        settle(&mut underway, accepted("close"));
        let partial = Event::OrderPartiallyFilled(OrderPartiallyFilled {
            order_client_id: "close".into(),
            fill: fill("close", 500.0),
            timestamp: 1,
        });
        assert_eq!(names(&settle(&mut underway, partial)), ["OrderPartiallyFilled"]);
        let settled = settle(&mut underway, filled("close", 500.0));
        assert_eq!(names(&settled), ["OrderPartiallyFilled", "OrderCanceled"]);
        let (Event::OrderPartiallyFilled(p), Some(Event::OrderCanceled(c))) = settled else {
            unreachable!()
        };
        assert_eq!(p.fill.last_qty, 500.0, "the fill itself is untouched");
        assert!(c.reason.contains("closed 1000 of this order's 2000 units"), "{}", c.reason);
        assert!(c.reason.contains("other 1000 were NOT executed"), "{}", c.reason);

        // A close filled whole, and an entry filled short (not a reduce-only close), keep `OrderFilled`.
        underway.insert(
            "whole".into(),
            Underway::of(&OrderRequest { qty: 2000.0, ..market("whole", true, Market) }),
        );
        assert_eq!(names(&settle(&mut underway, filled("whole", 2000.0))), ["OrderFilled"]);
        assert_eq!(names(&settle(&mut underway, filled("entry", 1000.0))), ["OrderFilled"]);
    }

    /// The five feature words this client reads are the Java's own (`Proto.java`'s `FEATURES`). The Java is read at run
    /// time (the public mirror ships no `jforex-bridge/` sources a test may `include_str!`), and a checkout without it
    /// says so, as `feed_tests.rs`'s `the_capabilities_and_the_feature_word_agree_with_the_java` does for the price lane.
    #[test]
    fn the_adopt_take_profit_reconcile_and_trigger_entry_feature_words_agree_with_the_java() {
        let proto = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("jforex-bridge/src/main/java/vike/jforex/Proto.java");
        let Ok(java) = std::fs::read_to_string(&proto) else {
            eprintln!("SKIPPED: {} is not in this checkout", proto.display());
            return;
        };
        let features = java
            .lines()
            .find(|l| l.contains("String[] FEATURES"))
            .expect("Proto.java declares FEATURES");
        for word in [
            FEATURE_MODIFY,
            FEATURE_ADOPT,
            FEATURE_TAKE_PROFIT,
            FEATURE_RECONCILE,
            FEATURE_TRIGGER_ENTRY,
        ] {
            assert!(features.contains(&format!("\"{word}\"")), "{word} missing from {features}");
        }
    }

    /// Every refusal while no sidecar runs carries the sidecar's reason, the failed restarts and the next try.
    #[test]
    fn a_down_sidecar_gives_its_reason_and_its_restart_in_the_words_of_a_refusal() {
        assert_eq!(down_words(&VenueLinkState::Up), None);
        let down = sidecar_down("JForex session disconnected".into(), 2, Duration::from_secs(20));
        let VenueLinkState::Down(VenueLinkDown { effect, .. }) = &down else { unreachable!() };
        assert_eq!(
            *effect, "",
            "the sidecar states no effect words: the wire's `effect` stays empty"
        );
        let words = down_words(&down).expect("down has words");
        assert!(words.contains("JForex session disconnected"), "{words}");
        assert!(words.contains("2 failure(s) so far"), "{words}");
        assert!(words.contains("20s"), "{words}");
    }

    /// **A dead sidecar is a failure from its death on** (`Error`, which the tradehub says at once): the first death,
    /// before any restart failed, reads `Error` too, where its `0` used to read as a routine reopen, said one ask late.
    #[test]
    fn a_dead_sidecar_reads_error_from_the_first_death() {
        use vike_model::feed_status::DataClientConnectionState::Error;
        let first = sidecar_down("JForex session disconnected".into(), 0, Duration::from_secs(5));
        assert_eq!(first.connection_state(), Error, "{first:?}");
        let VenueLinkState::Down(VenueLinkDown { failed_restarts, retry_in, .. }) = first else {
            unreachable!()
        };
        assert_eq!((failed_restarts, retry_in), (1, Duration::from_secs(5)), "the death alone");
        let later = sidecar_down("restart refused".into(), 3, Duration::from_secs(40));
        assert_eq!(later.connection_state(), Error);
        assert!(matches!(later, VenueLinkState::Down(VenueLinkDown { failed_restarts: 3, .. })));
    }

    /// **The kind is the `fatal`'s failure WORD, whatever its reason says**: a jar that rewords a reason changes no
    /// verdict. Only a word that says the sidecar never REACHED Dukascopy's servers is a kind a later start can pass;
    /// a login the venue refused parks, and so does a fatal with no word (a JVM error, missing settings).
    #[test]
    fn the_failure_word_decides_the_kind_not_the_reasons_words() {
        let word = |failure: StartFailure| Some(failure.word());
        for (reason, class, failure, message, kind) in [
            (
                "reworded: no descriptor",
                None,
                word(StartFailure::DescriptorUnreachable),
                None,
                ErrorKind::ServerError,
            ),
            (
                "reworded: the poll ran out",
                None,
                word(StartFailure::LoginTimedOut),
                None,
                ErrorKind::Timeout,
            ),
            (
                "reworded",
                Some("java.net.UnknownHostException"),
                word(StartFailure::LoginFailed),
                Some("platform.dukascopy.com"),
                ErrorKind::Network,
            ),
            (
                "reworded",
                None,
                word(StartFailure::LoginFailed),
                Some("Connection refused"),
                ErrorKind::Network,
            ),
            (
                "reworded",
                None,
                word(StartFailure::LoginFailed),
                Some("platform.dukascopy.com"),
                ErrorKind::Network,
            ),
            (
                "reworded",
                Some("java.lang.IllegalStateException"),
                word(StartFailure::LoginFailed),
                Some("Wrong login or password"),
                ErrorKind::Unknown,
            ),
            ("reworded", None, word(StartFailure::LoginFailed), None, ErrorKind::Unknown),
            // The words an older jar's verdicts were read from mean nothing beside a word that says otherwise.
            (
                "JForex platform descriptor (JNLP) could not be fetched after 6 attempts: x.jnlp",
                None,
                word(StartFailure::LoginFailed),
                Some("Wrong login or password"),
                ErrorKind::Unknown,
            ),
        ] {
            assert_eq!(
                fatal_kind(reason, class, failure, message),
                kind,
                "{reason} / {failure:?} / {message:?}"
            );
        }
        // A word this reader does not know is no word: the reason's words are read, as for an older jar.
        assert_eq!(
            fatal_kind("login timed out", None, Some("a_newer_word"), None),
            ErrorKind::Timeout
        );
        assert_eq!(fatal_kind("anything", None, Some("a_newer_word"), None), ErrorKind::Unknown);
    }

    /// **DK-5: a login that threw is decided on the jar's word alone**: `login_unreachable` (the jar found the JDK's
    /// failure of a socket that reached no server in the cause chain) is started again whatever its class and words
    /// say, `login_refused` parks whatever they say. The class and the JDK's words used to decide, the words when the
    /// SDK wrapped the failure in a class of its own.
    #[test]
    fn a_login_that_threw_is_decided_on_the_jars_word_never_its_class_or_words() {
        let unreachable = Some(StartFailure::LoginUnreachable.word());
        let refused = Some(StartFailure::LoginRefused.word());
        for (class, message) in
            [(None, None), (Some("com.dukascopy.SomeWrapper"), Some("Wrong login or password"))]
        {
            assert_eq!(fatal_kind("reworded", class, unreachable, message), ErrorKind::Network);
        }
        for (class, message) in [
            (None, None),
            (Some("java.net.ConnectException"), Some("Connection refused")),
            (None, Some("platform.dukascopy.com")),
        ] {
            assert_eq!(fatal_kind("login failed: x", class, refused, message), ErrorKind::Unknown);
        }
    }

    /// A jar older than the failure word (shortcut, `fatal_kind`'s doc): only a `fatal` whose words say the sidecar
    /// never REACHED Dukascopy's servers is a kind a later start can pass; a refused login, a corrupted cache, missing
    /// settings and a JVM error park.
    #[test]
    fn only_a_fatal_that_never_reached_dukascopy_is_started_again() {
        for (reason, kind) in [
            (
                "JForex platform descriptor (JNLP) could not be fetched after 6 attempts: x.jnlp",
                ErrorKind::ServerError,
            ),
            ("login timed out", ErrorKind::Timeout),
            ("login failed: Connection refused", ErrorKind::Network),
            ("login failed: dukascopy.com: Name or service not known", ErrorKind::Network),
            ("login failed: Read timed out", ErrorKind::Network),
            // `java.net.UnknownHostException`: its whole message is the host it could not resolve.
            ("login failed: platform.dukascopy.com", ErrorKind::Network),
            ("login failed: jnlp.dukascopy.com", ErrorKind::Network),
            ("login failed: 192.0.2.10", ErrorKind::Network),
        ] {
            assert_eq!(fatal_kind(reason, None, None, None), kind, "{reason}");
            assert!(!kind.is_terminal(), "{reason}");
        }
        // The class a jar of this change names beside the reason: a JDK network class is Network whatever the SDK's
        // words, any other class leaves the verdict to the words.
        for (reason, class, kind) in [
            (
                "login failed: the SDK's own words",
                "java.net.SocketTimeoutException",
                ErrorKind::Network,
            ),
            (
                "login failed: platform.dukascopy.com",
                "java.net.UnknownHostException",
                ErrorKind::Network,
            ),
            ("login failed: Connection refused", "com.dukascopy.Wrapped", ErrorKind::Network),
            (
                "login failed: Wrong login or password",
                "java.lang.IllegalStateException",
                ErrorKind::Unknown,
            ),
            (
                "JForex sidecar failed before the handshake",
                "java.net.SocketException",
                ErrorKind::Unknown,
            ),
        ] {
            assert_eq!(fatal_kind(reason, Some(class), None, None), kind, "{reason} / {class}");
        }
        for reason in [
            "login failed: Wrong login or password for user demo",
            "login failed: /state/jforex/JForex/.cache/x (No such file or directory)",
            // A dotted name that is no host: a class the corrupted cache could not load, and a bare word.
            "login failed: com.dukascopy.api.impl.Platform",
            "login failed: null",
            "missing DUKASCOPY_LOGIN / DUKASCOPY_PASSWORD / DUKASCOPY_JNLP env",
            "JForex mount failed: java.lang.Exception: refused — that was attempt 2",
            "JForex sidecar failed before the handshake with a JVM Error rather than an exception",
            "login failed (scripted)",
        ] {
            assert_eq!(
                fatal_kind(reason, None, None, None),
                ErrorKind::Unknown,
                "{reason} must park"
            );
        }
    }

    // A cancel or an amend whose outcome JForex left open (`Envelope::Unsettled`): what a venue read says of it.

    fn listed(
        order_client_id: &str,
        state: &str,
        side: i32,
        command: &str,
    ) -> crate::transport::ReconcileOrder {
        crate::transport::ReconcileOrder {
            order_id: "O1".into(),
            order_client_id: Some(order_client_id.into()),
            symbol: "EURUSD".into(),
            side,
            state: state.into(),
            command: command.into(),
            order_type: None,
            amount: 0.0,
            requested: 2000.0,
            price: 1.1,
            stop_loss: 0.0,
            take_profit: 0.0,
            commission: 0.0,
            created: 1,
            filled: 0,
        }
    }

    fn venue(orders: Vec<crate::transport::ReconcileOrder>) -> ReconcileAnswer {
        ReconcileAnswer { orders, positions: vec![], foreign: 0, timestamp: 1 }
    }

    fn asked(qty: Option<f64>, price: Option<f64>, trigger: Option<f64>) -> AmendAsked {
        AmendAsked { new_qty: qty, new_price: price, new_trigger_price: trigger }
    }

    /// The row of `order_client_id` among the venue's RESTING orders, as `settle_unsettled` reads it.
    fn resting(answer: &ReconcileAnswer, order_client_id: &str) -> vike_model::OrderStatusReport {
        crate::reconcile::order_reports(answer, &[])
            .into_iter()
            .find(|r| r.order_client_id.as_deref() == Some(order_client_id))
            .expect("the order rests")
    }

    /// **An amend is read back only on a term the venue's row shows**: a plain limit's price and every order's size,
    /// a conditional entry's trigger; never the limit of a stop-limit (a slippage the SDK states no getter for), so that
    /// read can never call the amend done.
    #[test]
    fn an_unsettled_amend_reads_back_only_terms_the_venue_row_shows() {
        let answer = venue(vec![listed("lim1", "OPENED", 1, "BUYLIMIT")]);
        let row = resting(&answer, "lim1");
        assert_eq!(asked(None, Some(1.1), None).read_back(&row), Some(true));
        assert_eq!(
            asked(None, Some(1.2), None).read_back(&row),
            Some(false),
            "the old price: not (yet)"
        );
        assert_eq!(asked(Some(2000.0), Some(1.1), None).read_back(&row), Some(true));
        assert_eq!(asked(Some(1000.0), Some(1.1), None).read_back(&row), Some(false));
        assert!(asked(None, None, None).read_back(&row).is_none(), "nothing asked is nothing read");

        let mut stop_limit = listed("sl1", "OPENED", 1, "BUYSTOP");
        stop_limit.order_type = Some(vike_model::OrderType::StopLimit);
        stop_limit.price = 1.5;
        let answer = venue(vec![stop_limit]);
        let row = resting(&answer, "sl1");
        assert_eq!(
            asked(None, None, Some(1.5)).read_back(&row),
            Some(true),
            "the trigger is the open price"
        );
        assert!(
            asked(None, Some(1.5002), None).read_back(&row).is_none(),
            "its limit cannot be read"
        );
        assert!(asked(None, Some(1.5002), Some(1.5)).read_back(&row).is_none());
    }

    /// **A cancel took only where the read shows it**: a plain order listed in NO state (listed FILLED, it filled, and
    /// its fill message says so); a protection whose price is on none of the open orders facing it while one still is
    /// open (none open: the price may have FIRED, and that close's message reports the fill).
    #[test]
    fn an_unsettled_cancel_took_only_where_the_venue_read_shows_it() {
        assert!(!cancel_took(&venue(vec![listed("lim1", "OPENED", 1, "BUYLIMIT")]), "lim1", None));
        assert!(!cancel_took(&venue(vec![listed("lim1", "FILLED", 1, "BUY")]), "lim1", None));
        assert!(cancel_took(&venue(vec![]), "lim1", None));

        let stop = OrderRequest {
            order_client_id: "stop1".into(),
            venue: "dukascopy".into(),
            symbol: "EURUSD".into(),
            side: -1,
            qty: 1000.0,
            order_type: vike_model::OrderType::Stop,
            trigger_price: Some(1.09),
            reduce_only: true,
            ..Default::default()
        };
        let mut long = listed("entry1", "FILLED", 1, "BUY");
        long.stop_loss = 1.09;
        assert!(
            !cancel_took(&venue(vec![long.clone()]), "stop1", Some(&stop)),
            "the price still holds"
        );
        long.stop_loss = 0.0;
        assert!(
            cancel_took(&venue(vec![long]), "stop1", Some(&stop)),
            "the price is off the open order"
        );
        assert!(
            !cancel_took(&venue(vec![]), "stop1", Some(&stop)),
            "nothing open: fired or closed, not read as a cancel"
        );
    }

    /// The catalog id the jar writes on the refusals it answers a cancel or an amend with (`Proto.java`'s
    /// `REFUSAL_VENUE_REFUSED`) is the node's own word. The Java is read at run time, as the feature words' test does.
    #[test]
    fn the_jars_refusal_id_is_the_venue_refused_word() {
        let proto = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("jforex-bridge/src/main/java/vike/jforex/Proto.java");
        let Ok(java) = std::fs::read_to_string(&proto) else {
            eprintln!("SKIPPED: {} is not in this checkout", proto.display());
            return;
        };
        let line = java
            .lines()
            .find(|l| l.contains("static final String REFUSAL_VENUE_REFUSED"))
            .expect("Proto.java declares REFUSAL_VENUE_REFUSED");
        let word = vike_model::OrderRefusalId::VenueRefused.word();
        assert!(line.contains(&format!("\"{word}\"")), "{word} missing from {line}");
    }
}
