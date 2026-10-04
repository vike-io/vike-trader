//! `auto_redeem` — the opt-in, credential-gated, kill-switched auto-redeem poller (CTF auto-redeem,
//! docs/superpowers/specs/2026-07-11-ctf-redeem-design.md). Unattended real-money on-chain action —
//! default OFF, at-most-once via RedeemLedger, a halt-file kill switch.
//!
//! Wires Task 2 (`positions::{PositionsClient, redeemable}`), Task 3 (`RedeemLedger`), and
//! Task 4 (`redeem_relayer::submit_redeem`) into ONE safety-gated loop. This is the ONLY place
//! `submit_redeem` is ever called outside tests, so every guard lives here:
//! - **default OFF**: [`AutoRedeemPoller::spawn`] returns `None` unless creds are present AND its
//!   caller passes `enabled` — the thread never starts otherwise, and no composition root starts it
//!   (D4 of decision 0095: its values are parameters until something does).
//! - **kill switch**: [`kill_switch_tripped`] is checked EVERY tick before acting; either the
//!   caller's `halted` or the halt file existing skips that tick (the thread keeps running so a
//!   later halt-file removal resumes without a restart).
//! - **at-most-once**: [`redeem_once`] writes the PERMANENT ledger tombstone only on on-chain proof
//!   — see "Two phases" below. A relayer HTTP 2xx marks nothing.
//! - **winners only**: discovery filters on the PAYOUT, not on the data-api's `redeemable` flag —
//!   see "Only winners are submitted" below.
//! - **neg-risk routing**: discovery includes neg-risk winners; this module routes each position
//!   by its `neg_risk` flag — binary → the era's binary target, neg-risk → the era's neg-risk adapter
//!   with the per-slot AMOUNTS derived from the position (see [`neg_risk_amounts`]). ⚠ Those amounts
//!   are INERT on [`crate::exec_plane::settlement::redeem::Era::V2`], the era this poller submits under: that adapter reads
//!   the caller's own balances and ignores the `uint256[]`, so a V2 neg-risk redeem is byte-identical
//!   to a binary one and differs only by target — see [`crate::exec_plane::settlement::redeem_relayer::RedeemKind::call`].
//! - **collateral era**: every redeem is submitted under [`crate::exec_plane::settlement::redeem::Era::V2`] (pUSD). There is
//!   no era field on a `/positions` row, and Polymarket retired the V1 relayer path on 2026-07-17, so
//!   V2 is the only correct route for current activity — see [`ProdDeps::redeem`].
//!
//! ## Two phases: SUBMIT is not SETTLE
//!
//! The relayer answering HTTP 2xx means it accepted a signed batch (`{"state":"NEW"}`, usually with
//! no transaction hash yet) — not that the redemption was mined, and not that it succeeded. Because
//! the ledger is persisted, marking on a 2xx wrote a permanent tombstone on evidence that does not
//! prove the money moved: a dropped or reverted redemption was forfeited forever. So each tick runs
//! two phases:
//!
//! 1. **Confirm** ([`redeem_once`]'s sweep) — every in-flight redemption is checked against the
//!    chain through [`RedeemDeps::confirm`] ⇒ [`crate::exec_plane::settlement::redeem_confirm::classify_receipt`]. Proven ⇒
//!    the ledger settles. Reverted, or mined-without-our-`PayoutRedemption` ⇒ the row REOPENS and
//!    is retried. An RPC error changes nothing. This phase runs FIRST and does not depend on the
//!    data-api, so a discovery outage can never block confirmation.
//! 2. **Discover + submit** — the `/positions` → winner filter, skipping only SETTLED conditionIds,
//!    with a write-ahead ledger record ([`RedeemLedger::begin`]) written BEFORE the relayer is
//!    contacted.
//!
//! ## Only winners are submitted (`redeemable` is a RESOLUTION flag)
//!
//! Discovery used to filter on `Position.redeemable && size > 0`. That flag is set on EVERY position
//! of a resolved condition, losers included — measured on this repo's own mainnet wallet, where all
//! four held positions reported `redeemable: true` and exactly one paid (the table is pinned in
//! [`crate::exec_plane::settlement::positions`]'s module doc and its `live_wallet_fixture` tests). Filtering on it alone made
//! this poller submit four relayer calls for that wallet, three of them paying nothing and each
//! writing a permanent ledger tombstone for a non-event.
//!
//! So phase 2 now filters on the PAYOUT, through [`RedeemDeps::payout`] ⇒
//! [`crate::exec_plane::settlement::positions::redeemable_by`]. [`ProdDeps`] answers it from the CTF's own
//! `payoutNumerators` via a cached [`crate::exec_plane::settlement::chain::ChainOracle`] — the authoritative source, and the
//! DEFAULT. The cheap network-free alternative (the data-api's `curPrice`) is available only through
//! the explicitly-named [`ProdDeps::with_cur_price_winner_source`]; nothing selects it today, and
//! [`crate::exec_plane::settlement::positions::WinnerSource`]'s doc states what you give up by doing so.
//!
//! A verdict of [`crate::exec_plane::settlement::positions::Payout::Unknown`] (RPC blip, contradictory chain state) submits
//! NOTHING. That is a one-tick delay, never a forfeit: the position stays `redeemable` until it is
//! actually redeemed, so the next pass re-offers it.
//!
//! ⚠ Consequence, stated once: an unreachable Polygon RPC now stalls DISCOVERY as well as
//! confirmation. Both fail closed — nothing is submitted and nothing is tombstoned — so the poller
//! idles rather than misbehaving, but a permanently-unreachable RPC means nothing is ever redeemed.
//!
//! ## Which failure a crash prefers
//!
//! **A bounded, delayed duplicate submit — never a forfeit.** The write-ahead record means a crash
//! anywhere in the submit window leaves `Pending{tx_hash: None}`, which suppresses re-submission
//! for [`PENDING_UNKNOWN_TIMEOUT_MS`] and then allows exactly one more attempt. If the pre-crash
//! POST had in fact landed, the position is no longer `redeemable` and discovery never re-offers it,
//! so the retry does not happen at all; if it did happen, `redeemPositions` burns a balance that is
//! already zero and pays nothing (CTF) or reverts (neg-risk) — a duplicate submit is a wasted
//! relayer call, not a double payout. `redeem_ledger`'s module doc carries the full argument.
//!
//! A **relayer error** is treated the same way, deliberately: `Err` from a POST cannot distinguish
//! "the relayer rejected it" from "the relayer accepted it and the connection died", so the pending
//! window stands and the retry waits. The cost is a delayed retry of a genuinely-rejected submit;
//! settlement is a human-timescale event and the poll interval is minutes, so that is cheap.
//!
//! ## Known residual (declared, not hidden)
//!
//! A pending row with NO transaction hash whose position then vanishes from `/positions` — the
//! shape a crashed-but-landed submit leaves — stays `Pending` forever. It is INERT (phase 1 has no
//! hash to look up, and phase 2 never sees the conditionId again, so it is never re-submitted and
//! never re-alerts), but the ledger also never records the truth about it. Closing it properly
//! needs the relayer to hand back a `transactionHash` on `/submit`, which is exactly one of the two
//! things `tests/redeem_smoke.rs` is now instrumented to answer on the arbdub live-verify run. If
//! it turns out the relayer never returns one, the follow-up is to poll the relayer by its
//! `transactionID` — a new wire contract, and deliberately not guessed at here.
//!
//! ⚠ Starting this poller therefore also requires reachable Polygon RPC (the keyless
//! [`crate::exec_plane::settlement::chain::DEFAULT_RPC_URL`] by default). It does NOT require the
//! chain watcher.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use vike_bridge_core::poller::{STOP_POLL_SLICE, StopHandle, sleep_stop_aware, spawn_poller};

use crate::config::PolymarketCreds;
use crate::exec_plane::settlement::chain::{ChainOracle, ChainRpcSettings};
use crate::exec_plane::settlement::positions::{
    Payout, Position, PositionsClient, WinnerSource, payout_of, redeemable_by,
};
use crate::exec_plane::settlement::redeem::Era;
use crate::exec_plane::settlement::redeem_confirm::{ChainRedeemConfirmer, RedeemConfirmation};
use crate::exec_plane::settlement::redeem_ledger::RedeemLedger;
use crate::exec_plane::settlement::redeem_relayer::{RedeemKind, RedeemResult, submit_redeem};

/// Bounded-failure policy: a conditionId is retried at most this many times per poller SESSION.
/// After `MAX_CONSECUTIVE_FAILURES` consecutive failed redeem attempts the poller adds the cid to
/// its session-local `disabled_this_session` set, and [`redeem_once`]'s `exclude` filter then keeps
/// it out of every subsequent tick — so it stops retrying that cid for the rest of the session (no
/// per-cid delay timer; just try-K-then-stop). Nothing about failures is persisted, so a process
/// restart resets the count and retries the cid again (settlements ARE persisted, via the ledger).
///
/// A REVERTED on-chain redemption counts toward this bound exactly like a failed submit: both are
/// "this attempt did not settle", and a conditionId whose redeem keeps reverting (a bad neg-risk
/// amount, a wrong era) must stop burning relayer calls.
const MAX_CONSECUTIVE_FAILURES: u32 = 5;

/// How long a pending redemption with NO usable transaction hash suppresses a re-submit.
///
/// This is the crash window and the `{"state":"NEW"}`-with-no-hash window, which are deliberately
/// indistinguishable. Ten minutes is far longer than any realistic relayer queue, and the re-submit
/// it eventually allows only fires for a conditionId the data-api STILL reports as `redeemable` —
/// i.e. exactly the case where the first submit did not land and a retry is correct.
pub const PENDING_UNKNOWN_TIMEOUT_MS: i64 = 10 * 60 * 1000;

/// How long a pending redemption WITH a transaction hash but no receipt waits before the
/// transaction is presumed dropped from the mempool and the row is reopened for a re-submit.
///
/// Only [`RedeemConfirmation::Unmined`] ages against this. A `Maturing` transaction — one we can
/// SEE mined, just not yet buried [`crate::exec_plane::settlement::redeem_confirm::MIN_CONFIRMATIONS`] deep — never does,
/// which is why those are separate variants.
pub const PENDING_TX_TIMEOUT_MS: i64 = 30 * 60 * 1000;

/// A CONFIRMED redemption paying at or below this (USDC) is logged at `warn` instead of passing
/// silently — see the `Settled` arm of [`confirm_pending`] for why a zero-paying winner is a
/// contradiction rather than a normal outcome.
///
/// One cent, chosen to sit far above the failure mode it exists to catch and far below any real
/// position: an index-set-shaped `uint256[]` sent to a contract that CONSUMES it (the retired
/// [`crate::exec_plane::settlement::redeem::Era::V1`] adapter) redeems `[1, 2]` BASE UNITS, i.e. `0.000003` USDC — three
/// orders of magnitude under this line — while a genuine winner pays its full share count. It is
/// an alarm threshold, so a false positive on a sub-cent dust position costs one log line.
pub const ZERO_PAYOUT_ALERT_USDC: f64 = 0.01;

/// Test seam: everything the loop needs from the outside world. `ProdDeps` is the real
/// `PositionsClient`/`submit_redeem`/`PolygonRpc` wiring; tests inject a scripted stub — no network
/// in tests.
pub trait RedeemDeps {
    fn list_positions(&self, proxy: &str) -> Result<Vec<Position>, String>;
    fn redeem(&self, proxy: &str, cid: &str, kind: &RedeemKind) -> Result<RedeemResult, String>;
    /// Read the CHAIN for proof that a submitted redemption actually happened. `Err` is a transport
    /// verdict only — the caller must leave the ledger untouched on one, never infer a settlement
    /// or a failure from an RPC blip.
    fn confirm(&self, condition_id: &str, tx_hash: &str) -> Result<RedeemConfirmation, String>;
    /// **The winner check.** What this position is actually worth at redemption — see "Only winners
    /// are submitted" in this module's doc for why the data-api's `redeemable` flag cannot answer
    /// this. Deliberately has NO default implementation: a default would be either
    /// [`Payout::Winner`] (re-introducing the exact defect) or [`Payout::Unknown`] (a poller that
    /// silently redeems nothing), so every implementor must state its source out loud.
    fn payout(&self, p: &Position) -> Payout;
}

/// Which winner source a [`ProdDeps`] consults. Owned twin of [`WinnerSource`], which borrows.
enum WinnerConfig {
    /// The DEFAULT: the CTF's own `payoutNumerators`, cached per resolved condition.
    Chain(ChainOracle),
    /// The explicit opt-out — see [`ProdDeps::with_cur_price_winner_source`].
    CurPriceOnly,
}

/// The production `RedeemDeps`: real data-api discovery, the authoritative on-chain winner check, a
/// real signed relayer submit, and the read-only on-chain confirmation that decides whether the
/// ledger may ever record it as done.
pub struct ProdDeps {
    pub creds: PolymarketCreds,
    /// Read-only Polygon receipt reader — never signs, never sends a transaction.
    pub confirmer: ChainRedeemConfirmer,
    /// Where [`RedeemDeps::payout`] reads winner truth from. Private so the safe default cannot be
    /// swapped by a struct literal — only by naming [`ProdDeps::with_cur_price_winner_source`].
    winner: WinnerConfig,
}

impl ProdDeps {
    /// The normal construction: creds, a confirmer, and the AUTHORITATIVE on-chain winner source,
    /// both RPC readers built from the caller's [`ChainRpcSettings`]. Opens no socket (both RPC
    /// clients dial lazily), and adds no new setting.
    pub fn new(creds: PolymarketCreds, chain: &ChainRpcSettings) -> Self {
        ProdDeps {
            creds,
            confirmer: ChainRedeemConfirmer::from_settings(chain),
            // Its own `ChainOracle`, deliberately not shared with the confirmer — the same
            // "each reader owns its client" discipline `redeem_confirm` already documents. The
            // oracle caches resolved verdicts, so a settled watchlist costs zero `eth_call`s/tick.
            winner: WinnerConfig::Chain(ChainOracle::from_settings(chain)),
        }
    }

    /// ⚠ **Opt out of the on-chain winner check** and decide winners from the data-api's `curPrice`
    /// instead. Network-free, and strictly weaker: `curPrice` is an off-chain indexer's display
    /// mark, not the payout the redeem contract reads. [`WinnerSource`]'s module doc states exactly
    /// what is given up. Intended for a host that genuinely cannot reach Polygon RPC; **nothing in
    /// this workspace calls it**, and it is spelled out at the call site by construction.
    pub fn with_cur_price_winner_source(mut self) -> Self {
        self.winner = WinnerConfig::CurPriceOnly;
        self
    }
}

impl RedeemDeps for ProdDeps {
    fn list_positions(&self, proxy: &str) -> Result<Vec<Position>, String> {
        PositionsClient::list(proxy)
    }
    fn payout(&self, p: &Position) -> Payout {
        match &self.winner {
            WinnerConfig::Chain(oracle) => {
                let lookup = |cid: &str| oracle.resolution(cid);
                payout_of(p, &WinnerSource::Chain(&lookup))
            }
            WinnerConfig::CurPriceOnly => payout_of(p, &WinnerSource::CurPriceOnly),
        }
    }
    fn redeem(&self, proxy: &str, cid: &str, kind: &RedeemKind) -> Result<RedeemResult, String> {
        // `kind` is decided per-position in `redeem_once` (binary CTF vs. neg-risk with the per-slot
        // amounts derived from the Position); we just forward it to the signed relayer.
        //
        // Era: ALWAYS [`Era::V2`] (pUSD). The data-api `/positions` row carries NO era discriminator
        // (see `crate::exec_plane::settlement::redeem::Era`), so the poller cannot tell a legacy USDC.e position from a
        // current pUSD one — and it does not need to: Polymarket retired the V1 relayer path on
        // 2026-07-17, so a V1 relayer call would fail on-chain regardless. Every current redeemable
        // position is V2; a genuinely-legacy USDC.e position (if any still redeemed by relayer)
        // cannot be auto-detected here and would need manual handling with `Era::V1`.
        submit_redeem(&self.creds, proxy, cid, kind, Era::V2)
    }
    fn confirm(&self, condition_id: &str, tx_hash: &str) -> Result<RedeemConfirmation, String> {
        self.confirmer.confirm(condition_id, tx_hash)
    }
}

/// Polymarket conditional tokens are USDC-collateralized and use **6 decimals**, so a human-readable
/// token balance (`Position::size`) converts to base units by `size * 1e6`.
const CTF_TOKEN_DECIMALS_SCALE: f64 = 1_000_000.0;

/// Derive the NegRiskAdapter `redeemPositions(bytes32, uint256[])` per-slot AMOUNTS array from a
/// redeemable neg-risk `Position`.
///
/// **PINNED (Task 1, see `redeem.rs` module doc):** the NegRiskAdapter's `uint256[]` is **per-slot
/// AMOUNTS (the redeemer's actual YES/NO conditional-token balances), NOT an index-set `[1, 2]`.**
/// Passing `[1, 2]` would redeem 1–2 base-units instead of the real balance — a broken redeem. So
/// this builds the amounts from the position, never a constant.
///
/// **ASSUMED — VERIFY AGAINST A LIVE `NegRiskAdapter` REDEEM VIA ARBDUB BEFORE ANYTHING STARTS THIS
/// POLLER.** Two derivation choices below are ASSUMPTIONS the live-verify must confirm (the feature
/// is default-OFF and fails closed — a wrong amount reverts on-chain, and since the fix that added
/// [`RedeemDeps::confirm`] a revert is now SEEN and retried rather than tombstoned, so nothing is
/// misdirected and nothing is silently forfeited — but do NOT start it until arbdub confirms
/// both):
///   1. **2-slot layout.** A neg-risk market's `conditionId` is a binary sub-position with 2 slots,
///      so the array is 2 elements; the position's own slot is `outcome_index` (clamped to 0/1) and
///      the other slot is `0`.
///   2. **6-decimal conversion.** base-unit amount = `round(size * 1e6)` (USDC-collateralized CTF).
///
/// `size → u128` is panic-free: NaN/negative/overflow all saturate (NaN→0, negative→0), so a garbage
/// size yields `0` (a safe no-op amount) rather than a panic. An UNREADABLE `outcome_index`
/// (`None` — the data-api omitted or garbled `outcomeIndex`) takes the same all-zero exit, for the
/// same reason: there is no slot to address and guessing one would point the redeem at the wrong
/// leg. That path is unreachable from the poller — [`payout_of`] returns
/// [`Payout::Unknown`] for a `None` slot under EVERY winner source, so such a position never becomes
/// a `Payout::Winner` and never gets here — but this function must not invent a slot on its own.
fn neg_risk_amounts(p: &Position) -> Vec<u128> {
    let base_units = (p.size * CTF_TOKEN_DECIMALS_SCALE).round();
    // `as u128` on f64 saturates (NaN → 0, negatives → 0, > u128::MAX → u128::MAX) — no panic.
    let amount = if base_units.is_finite() && base_units > 0.0 { base_units as u128 } else { 0 };
    let mut amounts = vec![0u128; 2];
    // No readable slot ⇒ leave the array all-zero (the same safe no-op a garbage size yields).
    let Some(slot) = p.outcome_index.map(|i| (i as usize).min(1)) else { return amounts };
    amounts[slot] = amount;
    amounts
}

/// One `redeem_once` pass's outcome. Note the split that the pre-fix `redeemed` field papered over:
/// **`submitted` is not `settled`** — a conditionId reaches `submitted` on a relayer 2xx and only
/// reaches `settled` on an on-chain receipt carrying its `PayoutRedemption`, typically several ticks
/// later.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct RedeemTickReport {
    /// Handed to the relayer this tick (write-ahead record written, POST accepted). NOT money yet.
    pub submitted: Vec<String>,
    /// PROVEN on-chain this tick and permanently ledger-settled. The only "done" list.
    pub settled: Vec<String>,
    /// Submitted earlier and still unresolved — no action taken this tick, no re-submit.
    pub awaiting: Vec<String>,
    /// The submit call itself failed this tick (relayer error). Left pending; retried after
    /// [`PENDING_UNKNOWN_TIMEOUT_MS`].
    pub failed: Vec<String>,
    /// The chain says the redemption did NOT happen (reverted / no matching `PayoutRedemption`), or
    /// the transaction was presumed dropped. Reopened — eligible again next tick.
    pub reverted: Vec<String>,
}

/// The kill switch: the caller's `halted` OR `halt_file` existing on disk, checked every tick — the
/// FILE is the runtime half, halting (or, removed, resuming) a running poller without a restart.
pub fn kill_switch_tripped(halted: bool, halt_file: &Path) -> bool {
    halted || halt_file.exists()
}

/// **Phase 1 — confirm.** Resolve every in-flight redemption against the chain, BEFORE (and
/// independently of) discovery, so a data-api outage cannot block confirmation and a conditionId
/// whose position has already vanished from `/positions` still gets its verdict.
///
/// Note this sweep ignores `exclude`: the session-disabled set must stop new SUBMITS, never stop a
/// pending redemption from being confirmed.
fn confirm_pending(
    deps: &dyn RedeemDeps,
    ledger: &RedeemLedger,
    now_ms: i64,
    report: &mut RedeemTickReport,
) {
    for (cid, pending) in ledger.pending_entries() {
        let Some(tx) = pending.tx_hash.as_deref() else {
            // No hash to look up — the crash window and the `{"state":"NEW"}` window. Discovery's
            // PENDING_UNKNOWN_TIMEOUT_MS is what eventually re-offers it.
            report.awaiting.push(cid);
            continue;
        };
        match deps.confirm(&cid, tx) {
            Ok(RedeemConfirmation::Settled { block, payout_usdc }) => {
                ledger.settle(&cid, Some(tx), block);
                tracing::info!(
                    condition_id = %cid, tx_hash = %tx, block, payout_usdc,
                    "auto_redeem: redemption CONFIRMED on-chain — ledger settled"
                );
                // A settlement that paid ~NOTHING is a contradiction worth shouting about, not a
                // number buried in an info line. Every conditionId that reaches a submit is a
                // WINNER by construction (phase 2 filters on [`RedeemDeps::payout`], never the
                // data-api's `redeemable` flag — the #974 fix), so a winner redeeming for zero
                // means one of the two things this cluster exists to prevent has happened anyway:
                // winner selection put a LOSER through, or the calldata moved (almost) no tokens.
                // Deliberately NOT a control-flow change — the tombstone is correct either way.
                // The proof `RedeemConfirmation::Settled` carries (status 1 + a `PayoutRedemption`
                // for THIS conditionId + `MIN_CONFIRMATIONS` deep) is what makes the redemption
                // real; nothing is retryable about a real redemption that paid zero, and reopening
                // it would re-submit forever. This is the alarm, not a guard.
                if payout_usdc <= ZERO_PAYOUT_ALERT_USDC {
                    tracing::warn!(
                        condition_id = %cid, tx_hash = %tx, block, payout_usdc,
                        "auto_redeem: redemption settled for ~ZERO — a position we priced as a WINNER paid nothing; check winner selection and the redeem calldata"
                    );
                }
                report.settled.push(cid);
            }
            Ok(RedeemConfirmation::Reverted { reason }) => {
                ledger.reopen(&cid);
                tracing::warn!(
                    condition_id = %cid, tx_hash = %tx, %reason,
                    "auto_redeem: redemption did NOT happen on-chain — reopened for retry"
                );
                report.reverted.push(cid);
            }
            // Mined and proven, just not buried deep enough yet: WAIT. Never ages toward the
            // dropped-transaction timeout — we can see it on chain.
            Ok(v @ RedeemConfirmation::Maturing { .. }) => {
                tracing::debug!(condition_id = %cid, tx_hash = %tx, reason = %v.reason(), "auto_redeem: awaiting confirmations");
                report.awaiting.push(cid);
            }
            Ok(RedeemConfirmation::Unmined) => {
                if now_ms.saturating_sub(pending.since_ms) >= PENDING_TX_TIMEOUT_MS {
                    ledger.reopen(&cid);
                    tracing::warn!(
                        condition_id = %cid, tx_hash = %tx,
                        "auto_redeem: no receipt within the drop timeout — transaction presumed dropped, reopened for retry"
                    );
                    report.reverted.push(cid);
                } else {
                    report.awaiting.push(cid);
                }
            }
            Err(e) => {
                // A transport blip is NOT a verdict: leave the row exactly as it is.
                tracing::warn!(
                    condition_id = %cid, tx_hash = %tx, %e,
                    "auto_redeem: confirmation read failed — ledger untouched, retrying next tick"
                );
                report.awaiting.push(cid);
            }
        }
    }
}

/// ONE confirm -> discovery -> submit pass — the reusable core the poller thread calls on a timer.
///
/// **Phase 1** ([`confirm_pending`]) settles or reopens every already-submitted redemption against
/// the chain. **Phase 2** discovers positions via `deps.list_positions`, filters to WINNING (binary
/// and neg-risk) conditionIds — per [`RedeemDeps::payout`], NOT the data-api's `redeemable` flag,
/// see this module's doc — that are neither SETTLED in `ledger` nor in `exclude`, and submits each
/// one, writing the ledger's write-ahead record BEFORE the relayer is contacted. Nothing here writes
/// the permanent tombstone; only phase 1 can.
///
/// Each position is routed by its `neg_risk` flag — binary → [`RedeemKind::Binary`], neg-risk →
/// [`RedeemKind::NegRisk`] carrying the per-slot AMOUNTS from [`neg_risk_amounts`].
///
/// ⚠ Those amounts are **INERT on the era this poller actually submits under** ([`Era::V2`], see
/// below): the V2 `NegRiskCtfCollateralAdapter` reads the caller's own balances and ignores the
/// `uint256[]` entirely, so a neg-risk redeem is byte-identical to a binary one and differs only by
/// target contract. They are still computed and carried because the retired [`Era::V1`] encoder
/// genuinely consumes them — see [`RedeemKind::call`] and `redeem.rs`'s module doc (verified
/// against the deployed adapter, 2026-08-02).
///
/// **De-dupe by `condition_id` (CRITICAL — real money).** `redeemPositions([1,2])` settles the
/// WHOLE binary condition in one call regardless of which leg is held, but the data-api returns one
/// `Position` row PER outcome token, so a wallet holding BOTH legs (YES+NO — realistic for an
/// arb/MM bot) of a resolved market yields two rows with the SAME `condition_id`. Redeeming per-row
/// would submit the identical on-chain redeem twice in one tick. So each `condition_id` is redeemed
/// AT MOST ONCE per tick: an intra-tick `attempted` set skips any repeat of a cid already tried this
/// pass.
///
/// `exclude` is the poller's session-local bounded-failure set: cids that already hit
/// `MAX_CONSECUTIVE_FAILURES` are filtered out here so they stop reaching `deps.redeem`. Tests that
/// don't exercise the failure policy pass an empty set.
///
/// `now_ms` is injected rather than read from the clock so the pending windows are deterministic in
/// tests; [`AutoRedeemPoller::spawn`] passes the real wall clock.
pub fn redeem_once(
    deps: &dyn RedeemDeps,
    proxy: &str,
    ledger: &RedeemLedger,
    exclude: &HashSet<String>,
    now_ms: i64,
) -> RedeemTickReport {
    let mut report = RedeemTickReport::default();

    // ---- Phase 1: confirm what is already in flight (independent of the data-api) --------------
    confirm_pending(deps, ledger, now_ms, &mut report);

    // ---- Phase 2: discover + submit ------------------------------------------------------------
    let positions = match deps.list_positions(proxy) {
        Ok(ps) => ps,
        Err(e) => {
            tracing::warn!(%e, "auto_redeem: list_positions failed this tick");
            return report;
        }
    };

    // Only SETTLED cids are filtered out here — a pending one must still be visible so its window
    // can expire into a retry. Session-disabled cids are excluded alongside them.
    //
    // `deps.payout` is the WINNER check: a resolved-but-worthless leg (the majority of a real
    // wallet's `redeemable: true` rows) never reaches the relayer, and an unanswerable one is
    // skipped this pass rather than guessed at.
    let candidates = redeemable_by(
        &positions,
        |cid| ledger.is_settled(cid) || exclude.contains(cid),
        |p| deps.payout(p),
    );

    // One redeem per conditionId per tick: guard against both legs of one binary condition being
    // present as two Position rows (identical condition_id) → a single settling redeem, not two.
    let mut attempted: HashSet<String> = HashSet::new();
    for p in &candidates {
        if !attempted.insert(p.condition_id.clone()) {
            // already handled this exact condition this tick (the other leg) — skip the duplicate.
            continue;
        }

        // Is a submit still in flight for this condition?
        if let Some(pending) = ledger.pending(&p.condition_id) {
            if pending.tx_hash.is_some() {
                // Phase 1 owns this one: it settles it, or reopens it on a revert / drop timeout.
                // Never re-submit behind its back.
                continue;
            }
            if now_ms.saturating_sub(pending.since_ms) < PENDING_UNKNOWN_TIMEOUT_MS {
                continue; // inside the crash / no-hash window — already reported as awaiting.
            }
            tracing::warn!(
                condition_id = %p.condition_id, attempts = pending.attempts,
                "auto_redeem: submit window expired with no transaction hash — re-submitting (a duplicate submit cannot double-pay; see redeem_ledger's module doc)"
            );
        }

        // Route by the position's neg-risk flag: binary → CTF, neg-risk → NegRiskAdapter with the
        // per-slot AMOUNTS built from this Position (PINNED: amounts, NOT [1,2] — see redeem.rs).
        let kind =
            if p.neg_risk { RedeemKind::NegRisk(neg_risk_amounts(p)) } else { RedeemKind::Binary };

        // WRITE-AHEAD: the pending record is on disk (and fsynced) before the relayer sees
        // anything, so a crash in the submit window leaves evidence instead of a silent gap.
        // `None` means the ledger refused (already settled) — never contact the relayer then.
        let Some(attempt) = ledger.begin(&p.condition_id, now_ms) else {
            continue;
        };

        match deps.redeem(proxy, &p.condition_id, &kind) {
            Ok(res) => {
                if let Some(tx) = res.tx_hash.as_deref() {
                    ledger.attach_tx(&p.condition_id, tx);
                }
                tracing::info!(
                    condition_id = %p.condition_id, attempt, tx_hash = ?res.tx_hash,
                    "auto_redeem: redeem SUBMITTED to the relayer — awaiting on-chain confirmation (not settled)"
                );
                report.submitted.push(p.condition_id.clone());
            }
            Err(e) => {
                // AMBIGUOUS by construction: an error on the POST cannot distinguish "rejected" from
                // "accepted, then the connection died". The write-ahead pending record therefore
                // STANDS — the retry waits out PENDING_UNKNOWN_TIMEOUT_MS rather than firing on the
                // next tick.
                tracing::warn!(
                    condition_id = %p.condition_id, attempt, %e,
                    "auto_redeem: redeem submit failed — left pending (outcome unknown), retry after the submit window"
                );
                report.failed.push(p.condition_id.clone());
            }
        }
    }

    report
}

pub struct AutoRedeemPoller;

impl AutoRedeemPoller {
    /// Spawn the poller thread. Returns `None` (never starts anything) unless creds are present AND
    /// the caller passes `enabled` — the caller passes `creds: Option<PolymarketCreds>` from the
    /// normal absent-credentials-is-the-live-gate load path (`config::load_polymarket_creds_from`),
    /// and `enabled`, `halted` and `chain` are its own decisions (D4 of decision 0095: no
    /// composition root starts this poller, so it reads no setting of its own).
    ///
    /// The thread loop, every `interval`: check [`kill_switch_tripped`] with `halted` and
    /// `halt_file` — if tripped, skip acting this tick (loop keeps running so an operator can
    /// un-halt by removing the file, without a restart); else run one [`redeem_once`] pass against
    /// the real `ProdDeps`, whose RPC readers dial `chain`. A conditionId that fails or REVERTS
    /// `MAX_CONSECUTIVE_FAILURES` times in a row within this session is `error!`-logged and skipped
    /// for the rest of the session (nothing persisted — a restart retries it); an on-chain
    /// settlement clears its counter.
    #[allow(clippy::too_many_arguments)] // each is a separate caller decision (D4 of decision 0095)
    pub fn spawn(
        creds: Option<PolymarketCreds>,
        proxy: String,
        ledger_path: PathBuf,
        halt_file: PathBuf,
        interval: Duration,
        enabled: bool,
        halted: bool,
        chain: ChainRpcSettings,
    ) -> Option<StopHandle> {
        let creds = creds?;
        if !enabled {
            return None;
        }

        Some(spawn_poller("vike-polymarket-auto-redeem", move |stop| {
            let deps = ProdDeps::new(creds, &chain);
            let ledger = RedeemLedger::open(ledger_path);
            let mut consecutive_failures: HashMap<String, u32> = HashMap::new();
            // Bounded-failure policy: cids that hit MAX_CONSECUTIVE_FAILURES land here and are
            // passed as `exclude` to redeem_once, which keeps them out of every later SUBMIT (the
            // confirmation sweep still runs for them — an exclusion must not strand money that is
            // already in flight).
            let mut disabled_this_session: HashSet<String> = HashSet::new();

            while !stop.load(Ordering::Relaxed) {
                if kill_switch_tripped(halted, &halt_file) {
                    tracing::warn!("auto_redeem: kill switch tripped — skipping this tick");
                } else {
                    // The wall clock the pending windows run against.
                    let report = redeem_once(
                        &deps,
                        &proxy,
                        &ledger,
                        &disabled_this_session,
                        vike_model::now_ms(),
                    );
                    // Only an on-chain SETTLEMENT clears the counter: a submit that is accepted and
                    // then reverts must keep climbing toward the bound.
                    for cid in &report.settled {
                        consecutive_failures.remove(cid);
                    }
                    for cid in report.failed.iter().chain(report.reverted.iter()) {
                        let n = consecutive_failures.entry(cid.clone()).or_insert(0);
                        *n += 1;
                        if *n >= MAX_CONSECUTIVE_FAILURES
                            && disabled_this_session.insert(cid.clone())
                        {
                            tracing::error!(
                                condition_id = %cid,
                                failures = *n,
                                "auto_redeem: conditionId hit the consecutive-failure bound — skipping for the rest of this session"
                            );
                        }
                    }
                }
                if sleep_stop_aware(&stop, interval, STOP_POLL_SLICE) {
                    break;
                }
            }
        }))
    }
}

#[path = "auto_redeem_tests.rs"]
#[cfg(test)]
mod auto_redeem_tests;
