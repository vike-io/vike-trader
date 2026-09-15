//! `auto_redeem` — the opt-in, credential-gated, kill-switched auto-redeem poller (CTF auto-redeem,
//! docs/superpowers/specs/2026-07-11-ctf-redeem-design.md). Unattended real-money on-chain action —
//! default OFF, at-most-once via RedeemLedger, POLY_REDEEM_HALT kill switch.
//!
//! Wires Task 2 (`positions::{PositionsClient, redeemable}`), Task 3 (`RedeemLedger`), and
//! Task 4 (`redeem_relayer::submit_redeem`) into ONE safety-gated loop. This is the ONLY place
//! `submit_redeem` is ever called outside tests, so every guard lives here:
//! - **default OFF**: [`AutoRedeemPoller::spawn`] returns `None` unless creds are present AND
//!   [`auto_redeem_enabled`] (`POLY_AUTO_REDEEM=1`) — the thread never starts otherwise.
//! - **kill switch**: [`kill_switch_tripped`] is checked EVERY tick before acting; either
//!   `POLY_REDEEM_HALT` env (any value) or the halt file existing skips that tick (the thread keeps
//!   running so a later halt-file removal resumes without a restart).
//! - **at-most-once**: [`redeem_once`] writes the PERMANENT ledger tombstone only on on-chain proof
//!   — see "Two phases" below. A relayer HTTP 2xx marks nothing.
//! - **winners only**: discovery filters on the PAYOUT, not on the data-api's `redeemable` flag —
//!   see "Only winners are submitted" below.
//! - **neg-risk routing**: discovery includes neg-risk winners; this module routes each position
//!   by its `neg_risk` flag — binary → the era's binary target, neg-risk → the era's neg-risk adapter
//!   with the per-slot AMOUNTS derived from the position (see [`neg_risk_amounts`]). ⚠ Those amounts
//!   are INERT on [`crate::redeem::Era::V2`], the era this poller submits under: that adapter reads
//!   the caller's own balances and ignores the `uint256[]`, so a V2 neg-risk redeem is byte-identical
//!   to a binary one and differs only by target — see [`crate::redeem_relayer::RedeemKind::call`].
//! - **collateral era**: every redeem is submitted under [`crate::redeem::Era::V2`] (pUSD). There is
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
//!    chain through [`RedeemDeps::confirm`] ⇒ [`crate::redeem_confirm::classify_receipt`]. Proven ⇒
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
//! [`crate::positions`]'s module doc and its `live_wallet_fixture` tests). Filtering on it alone made
//! this poller submit four relayer calls for that wallet, three of them paying nothing and each
//! writing a permanent ledger tombstone for a non-event.
//!
//! So phase 2 now filters on the PAYOUT, through [`RedeemDeps::payout`] ⇒
//! [`crate::positions::redeemable_by`]. [`ProdDeps`] answers it from the CTF's own
//! `payoutNumerators` via a cached [`crate::chain::ChainOracle`] — the authoritative source, and the
//! DEFAULT. The cheap network-free alternative (the data-api's `curPrice`) is available only through
//! the explicitly-named [`ProdDeps::with_cur_price_winner_source`]; nothing selects it today, and
//! [`crate::positions::WinnerSource`]'s doc states what you give up by doing so.
//!
//! A verdict of [`crate::positions::Payout::Unknown`] (RPC blip, contradictory chain state) submits
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
//! ⚠ Enabling `POLY_AUTO_REDEEM=1` therefore now also requires reachable Polygon RPC (the keyless
//! [`crate::chain::DEFAULT_RPC_URL`] by default). It does NOT require `POLY_CHAIN_WATCH=1`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use vike_bridge_core::poller::{STOP_POLL_SLICE, StopHandle, sleep_stop_aware, spawn_poller};

use crate::chain::ChainOracle;
use crate::config::PolymarketCreds;
use crate::positions::{Payout, Position, PositionsClient, WinnerSource, payout_of, redeemable_by};
use crate::redeem::Era;
use crate::redeem_confirm::{ChainRedeemConfirmer, RedeemConfirmation};
use crate::redeem_ledger::RedeemLedger;
use crate::redeem_relayer::{RedeemKind, RedeemResult, submit_redeem};

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
/// SEE mined, just not yet buried [`crate::redeem_confirm::MIN_CONFIRMATIONS`] deep — never does,
/// which is why those are separate variants.
pub const PENDING_TX_TIMEOUT_MS: i64 = 30 * 60 * 1000;

/// A CONFIRMED redemption paying at or below this (USDC) is logged at `warn` instead of passing
/// silently — see the `Settled` arm of [`confirm_pending`] for why a zero-paying winner is a
/// contradiction rather than a normal outcome.
///
/// One cent, chosen to sit far above the failure mode it exists to catch and far below any real
/// position: an index-set-shaped `uint256[]` sent to a contract that CONSUMES it (the retired
/// [`crate::redeem::Era::V1`] adapter) redeems `[1, 2]` BASE UNITS, i.e. `0.000003` USDC — three
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
    /// all built from the existing `POLY_CHAIN_*` knobs. Opens no socket (both RPC clients dial
    /// lazily), and adds no new setting.
    pub fn new(creds: PolymarketCreds) -> Self {
        ProdDeps {
            creds,
            confirmer: ChainRedeemConfirmer::from_env(),
            // Its own `ChainOracle`, deliberately not shared with the confirmer — the same
            // "each reader owns its client" discipline `redeem_confirm` already documents. The
            // oracle caches resolved verdicts, so a settled watchlist costs zero `eth_call`s/tick.
            winner: WinnerConfig::Chain(ChainOracle::from_env()),
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
        // (see `crate::redeem::Era`), so the poller cannot tell a legacy USDC.e position from a
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

/// Unix milliseconds — the clock the pending windows below run against.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Derive the NegRiskAdapter `redeemPositions(bytes32, uint256[])` per-slot AMOUNTS array from a
/// redeemable neg-risk `Position`.
///
/// **PINNED (Task 1, see `redeem.rs` module doc):** the NegRiskAdapter's `uint256[]` is **per-slot
/// AMOUNTS (the redeemer's actual YES/NO conditional-token balances), NOT an index-set `[1, 2]`.**
/// Passing `[1, 2]` would redeem 1–2 base-units instead of the real balance — a broken redeem. So
/// this builds the amounts from the position, never a constant.
///
/// **ASSUMED — VERIFY AGAINST A LIVE `NegRiskAdapter` REDEEM VIA ARBDUB BEFORE `POLY_AUTO_REDEEM=1`.**
/// Two derivation choices below are ASSUMPTIONS the live-verify must confirm (the feature is
/// default-OFF and fails closed — a wrong amount reverts on-chain, and since the fix that added
/// [`RedeemDeps::confirm`] a revert is now SEEN and retried rather than tombstoned, so nothing is
/// misdirected and nothing is silently forfeited — but do NOT flip the opt-in until arbdub confirms
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

/// `POLY_AUTO_REDEEM=1` is the opt-in gate — default OFF (unset, empty, or any other value keeps
/// the poller from ever starting).
pub fn auto_redeem_enabled() -> bool {
    auto_redeem_enabled_in(std::env::var("POLY_AUTO_REDEEM").ok().as_deref())
}

/// The PURE gate over the value as read — `Some("1")` exactly. Split out so a test can drive it
/// without `std::env::set_var`, an `unsafe fn` since edition 2024 that this workspace forbids.
fn auto_redeem_enabled_in(value: Option<&str>) -> bool {
    value == Some("1")
}

/// The kill switch: `POLY_REDEEM_HALT` env present (any value, including empty) OR `halt_file`
/// exists on disk. Checked every tick — an operator can halt (or resume, by removing the file/env)
/// an already-running poller without restarting it.
pub fn kill_switch_tripped(halt_file: &Path) -> bool {
    kill_switch_tripped_in(std::env::var_os("POLY_REDEEM_HALT").is_some(), halt_file)
}

/// The PURE kill switch over the env PRESENCE bit as read (any value, including empty) OR the
/// file. Split out so a test can drive it without `std::env::set_var`, an `unsafe fn` since
/// edition 2024 that this workspace forbids.
fn kill_switch_tripped_in(env_present: bool, halt_file: &Path) -> bool {
    env_present || halt_file.exists()
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

/// Owner-side handle: stop-aware shutdown of the poller thread, `Drop`-joining — the shared
/// [`StopHandle`] scaffold (`vike_bridge_core::poller`), so a dropped handle never leaks the
/// background thread (the crate's discipline — see `raw_tap.rs`).
pub type AutoRedeemHandle = StopHandle;

pub struct AutoRedeemPoller;

impl AutoRedeemPoller {
    /// Spawn the poller thread. Returns `None` (never starts anything) unless creds are present AND
    /// [`auto_redeem_enabled`] — the caller passes `creds: Option<PolymarketCreds>` from the normal
    /// absent-credentials-is-the-live-gate load path (`config::load_polymarket_creds_from`).
    ///
    /// The thread loop, every `interval`: check [`kill_switch_tripped`] against `halt_file` — if
    /// tripped, skip acting this tick (loop keeps running so an operator can un-halt without a
    /// restart); else run one [`redeem_once`] pass against the real `ProdDeps`. A conditionId that
    /// fails or REVERTS `MAX_CONSECUTIVE_FAILURES` times in a row within this session is
    /// `error!`-logged and skipped for the rest of the session (nothing persisted — a restart
    /// retries it); an on-chain settlement clears its counter.
    pub fn spawn(
        creds: Option<PolymarketCreds>,
        proxy: String,
        ledger_path: PathBuf,
        halt_file: PathBuf,
        interval: Duration,
    ) -> Option<AutoRedeemHandle> {
        let creds = creds?;
        if !auto_redeem_enabled() {
            return None;
        }

        Some(spawn_poller("vike-polymarket-auto-redeem", move |stop| {
            let deps = ProdDeps::new(creds);
            let ledger = RedeemLedger::open(ledger_path);
            let mut consecutive_failures: HashMap<String, u32> = HashMap::new();
            // Bounded-failure policy: cids that hit MAX_CONSECUTIVE_FAILURES land here and are
            // passed as `exclude` to redeem_once, which keeps them out of every later SUBMIT (the
            // confirmation sweep still runs for them — an exclusion must not strand money that is
            // already in flight).
            let mut disabled_this_session: HashSet<String> = HashSet::new();

            while !stop.load(Ordering::Relaxed) {
                if kill_switch_tripped(&halt_file) {
                    tracing::warn!("auto_redeem: kill switch tripped — skipping this tick");
                } else {
                    let report =
                        redeem_once(&deps, &proxy, &ledger, &disabled_this_session, now_ms());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A scripted `RedeemDeps`: no network. `confirmations` is a queue popped front-to-back; the
    /// LAST entry repeats forever once reached (so a test states only the transitions it cares
    /// about). `tx_hash` is what the fake relayer returns on a 2xx — `None` reproduces the shape
    /// actually pinned from the reference SDK (`{"transactionID":"…","state":"NEW"}`, no hash).
    struct StubDeps {
        positions: Vec<Position>,
        /// records `(condition_id, is_neg_risk)` so tests can assert ROUTING, not just the cid.
        redeemed: Mutex<Vec<(String, bool)>>,
        tx_hash: Option<String>,
        confirmations: Mutex<Vec<RedeemConfirmation>>,
        /// Scripted winner verdicts by conditionId. A cid not listed is a `Winner`, so the tests
        /// that are about the ledger/confirmation state machine stay about that and say nothing
        /// about payouts; the payout-specific tests below list their cids explicitly.
        payouts: HashMap<String, Payout>,
    }

    impl StubDeps {
        fn new(positions: Vec<Position>) -> Self {
            StubDeps {
                positions,
                redeemed: Mutex::new(Vec::new()),
                tx_hash: Some("0xtx".into()),
                confirmations: Mutex::new(vec![RedeemConfirmation::Unmined]),
                payouts: HashMap::new(),
            }
        }
        /// The relayer answers 2xx with NO transaction hash (the pinned `state:"NEW"` shape).
        fn without_tx_hash(mut self) -> Self {
            self.tx_hash = None;
            self
        }
        fn scripted(self, script: Vec<RedeemConfirmation>) -> Self {
            *self.confirmations.lock().unwrap() = script;
            self
        }
        /// Pin the winner verdict for specific conditionIds (everything else stays `Winner`).
        fn with_payouts(mut self, rows: &[(&str, Payout)]) -> Self {
            self.payouts = rows.iter().map(|(c, v)| ((*c).to_string(), *v)).collect();
            self
        }
        fn submits(&self) -> Vec<(String, bool)> {
            self.redeemed.lock().unwrap().clone()
        }
    }

    fn settled(block: u64) -> RedeemConfirmation {
        RedeemConfirmation::Settled { block, payout_usdc: 12.5 }
    }
    fn reverted() -> RedeemConfirmation {
        RedeemConfirmation::Reverted { reason: "no PayoutRedemption".into() }
    }

    impl RedeemDeps for StubDeps {
        fn list_positions(&self, _proxy: &str) -> Result<Vec<Position>, String> {
            Ok(self.positions.clone())
        }
        fn redeem(
            &self,
            _proxy: &str,
            cid: &str,
            kind: &RedeemKind,
        ) -> Result<RedeemResult, String> {
            // Assert the neg-risk amounts array is non-empty when routed NegRisk (routing sanity —
            // the exact on-chain amount value is the arbdub live-verify item, not asserted here).
            if let RedeemKind::NegRisk(amounts) = kind {
                assert!(
                    !amounts.is_empty(),
                    "neg-risk redeem must carry a non-empty amounts array"
                );
            }
            self.redeemed
                .lock()
                .unwrap()
                .push((cid.to_string(), matches!(kind, RedeemKind::NegRisk(_))));
            Ok(RedeemResult { tx_hash: self.tx_hash.clone(), raw: "{\"state\":\"NEW\"}".into() })
        }
        fn confirm(
            &self,
            _condition_id: &str,
            _tx_hash: &str,
        ) -> Result<RedeemConfirmation, String> {
            let mut q = self.confirmations.lock().unwrap();
            if q.len() > 1 {
                Ok(q.remove(0))
            } else {
                Ok(q.first().cloned().unwrap_or(RedeemConfirmation::Unmined))
            }
        }
        fn payout(&self, p: &Position) -> Payout {
            self.payouts.get(&p.condition_id).copied().unwrap_or(Payout::Winner)
        }
    }

    fn pos(cid: &str, redeemable: bool, size: f64, neg: bool) -> Position {
        Position {
            condition_id: cid.into(),
            asset: "a".into(),
            size,
            redeemable,
            neg_risk: neg,
            outcome_index: Some(0),
            title: "t".into(),
            cur_price: Some(1.0),
        }
    }

    fn ledger(dir: &tempfile::TempDir) -> RedeemLedger {
        RedeemLedger::open(dir.path().join("r.jsonl"))
    }

    // =============================================================================================
    // THE FIX: a relayer 2xx does not settle; only on-chain proof does.
    // =============================================================================================

    /// **The regression guard for the bug this module was fixed for.** The relayer accepted the
    /// redeem (HTTP 2xx, a transaction hash in hand) and the chain has NOT confirmed it. The ledger
    /// must NOT be settled — the pre-fix code wrote its permanent tombstone right here, and a
    /// dropped or reverted transaction then forfeited the position forever.
    #[test]
    fn a_relayer_2xx_without_on_chain_confirmation_does_not_settle_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]);

        let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
        assert_eq!(rep.submitted, vec!["0x1".to_string()], "handed to the relayer");
        assert!(rep.settled.is_empty(), "NOTHING is settled by a 2xx");
        assert!(!l.is_settled("0x1"), "no permanent tombstone on an unconfirmed submit");
        assert_eq!(
            l.pending("0x1").map(|p| p.tx_hash),
            Some(Some("0xtx".to_string())),
            "recorded as in-flight, with the relayer's tx hash"
        );
    }

    /// The other half: once the chain proves it, the ledger settles permanently and survives a
    /// restart.
    #[test]
    fn a_confirmed_receipt_settles_the_ledger_permanently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        let deps =
            StubDeps::new(vec![pos("0x1", true, 100.0, false)]).scripted(vec![settled(90_715_029)]);
        {
            let l = RedeemLedger::open(path.clone());
            redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0); // submit
            let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 1_000); // confirm
            assert_eq!(rep.settled, vec!["0x1".to_string()]);
            assert!(l.is_settled("0x1"));
        }
        // the tombstone is persisted, and a later tick never re-submits.
        let l = RedeemLedger::open(path);
        assert!(l.is_settled("0x1"), "settlement survives a restart");
        let before = deps.submits().len();
        let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 2_000);
        assert!(rep.submitted.is_empty() && rep.settled.is_empty());
        assert_eq!(deps.submits().len(), before, "a settled condition is never re-submitted");
    }

    /// A CONFIRMED redemption that paid ~nothing still settles PERMANENTLY. This looks like the
    /// wrong answer and is the right one, so it is pinned: the alarm added alongside it is a `warn`,
    /// deliberately NOT a `reopen`. `Settled` already carries on-chain proof (status 1 + a
    /// `PayoutRedemption` for THIS conditionId + `MIN_CONFIRMATIONS` deep) — the redemption really
    /// happened, so there is nothing to retry, and reopening would re-submit the same call forever
    /// against a condition that has already paid out. What the zero payout indicts is the DECISION
    /// to redeem (winner selection) or the CALLDATA, neither of which a retry would change.
    #[test]
    fn a_zero_payout_settlement_still_tombstones_and_never_retries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        // Below ZERO_PAYOUT_ALERT_USDC: the shape a V1 index-set `[1, 2]` would have redeemed.
        let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]).scripted(vec![
            RedeemConfirmation::Settled { block: 90_715_029, payout_usdc: 0.000_003 },
        ]);
        let l = RedeemLedger::open(path);
        redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0); // submit
        let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 1_000); // confirm
        assert_eq!(rep.settled, vec!["0x1".to_string()], "settled, not reverted");
        assert!(rep.reverted.is_empty(), "a proven redemption is never reopened for retry");
        assert!(l.is_settled("0x1"), "the tombstone is written on proof, not on amount");
        let before = deps.submits().len();
        redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 2_000);
        assert_eq!(deps.submits().len(), before, "never re-submitted");
        // The threshold itself: far above the dust it catches, far below any real winner. `const`
        // blocks because these ARE compile-time facts (clippy::assertions_on_constants), which is
        // the stronger form anyway — a bad edit to the constant fails the BUILD, not just this test.
        const { assert!(0.000_003 < ZERO_PAYOUT_ALERT_USDC, "dust must trip the alarm") };
        const { assert!(ZERO_PAYOUT_ALERT_USDC < 1.0, "a one-share winner must not") };
    }

    /// Mined but not yet buried `MIN_CONFIRMATIONS` deep: wait, do not settle, do not re-submit.
    #[test]
    fn a_maturing_receipt_neither_settles_nor_resubmits() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)])
            .scripted(vec![RedeemConfirmation::Maturing { block: 90_715_029, confirmations: 4 }]);
        redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
        // far beyond BOTH timeouts — a transaction we can see mined never ages into "dropped".
        let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), PENDING_TX_TIMEOUT_MS * 10);
        assert_eq!(rep.awaiting, vec!["0x1".to_string()]);
        assert!(rep.settled.is_empty() && rep.reverted.is_empty() && rep.submitted.is_empty());
        assert!(!l.is_settled("0x1"));
        assert_eq!(deps.submits().len(), 1, "no re-submit while it matures");
    }

    /// A REVERTED transaction reopens the row and the next tick retries it — the failure mode that
    /// used to be a silent permanent forfeit.
    #[test]
    fn a_reverted_transaction_reopens_the_row_and_retries() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]).scripted(vec![reverted()]);
        redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0); // submit #1
        assert_eq!(deps.submits().len(), 1);

        let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 1_000);
        assert_eq!(rep.reverted, vec!["0x1".to_string()], "the chain said it did not happen");
        assert_eq!(rep.submitted, vec!["0x1".to_string()], "and it is re-submitted the same tick");
        assert_eq!(deps.submits().len(), 2);
        assert!(!l.is_settled("0x1"));
        // `reopen` clears the row outright, so the fresh submit starts a fresh window at attempt 1.
        // Cross-tick failure counting is the poller's session-local job, not the ledger's.
        assert_eq!(l.pending("0x1").map(|p| p.attempts), Some(1), "a fresh in-flight window");
    }

    /// A transaction hash that never gets a receipt is presumed DROPPED once past the timeout, and
    /// only then. Before the timeout the row is left strictly alone.
    #[test]
    fn an_unmined_transaction_is_reopened_only_after_the_drop_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]);
        redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);

        let early = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), PENDING_TX_TIMEOUT_MS - 1);
        assert_eq!(early.awaiting, vec!["0x1".to_string()]);
        assert!(early.reverted.is_empty());
        assert_eq!(deps.submits().len(), 1, "no re-submit inside the drop window");

        let late = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), PENDING_TX_TIMEOUT_MS);
        assert_eq!(late.reverted, vec!["0x1".to_string()]);
        assert_eq!(late.submitted, vec!["0x1".to_string()]);
        assert_eq!(deps.submits().len(), 2);
    }

    /// An RPC failure is a TRANSPORT verdict, never a settlement one: the row is untouched.
    #[test]
    fn a_confirmation_rpc_error_changes_nothing() {
        struct RpcDownDeps(StubDeps);
        impl RedeemDeps for RpcDownDeps {
            fn list_positions(&self, p: &str) -> Result<Vec<Position>, String> {
                self.0.list_positions(p)
            }
            fn redeem(&self, p: &str, c: &str, k: &RedeemKind) -> Result<RedeemResult, String> {
                self.0.redeem(p, c, k)
            }
            fn confirm(&self, _c: &str, _t: &str) -> Result<RedeemConfirmation, String> {
                Err("rpc eth_getTransactionReceipt: network: timed out".into())
            }
            fn payout(&self, p: &Position) -> Payout {
                self.0.payout(p)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let deps = RpcDownDeps(StubDeps::new(vec![pos("0x1", true, 100.0, false)]));
        redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
        let before = l.pending("0x1");
        let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 1_000);
        assert_eq!(rep.awaiting, vec!["0x1".to_string()]);
        assert!(rep.settled.is_empty() && rep.reverted.is_empty() && rep.submitted.is_empty());
        assert_eq!(l.pending("0x1"), before, "ledger row byte-identical after an RPC blip");
    }

    // =============================================================================================
    // The crash window — and which of the two errors this design prefers.
    // =============================================================================================

    /// **Crash mid-window, and the choice stated in the test name.** The write-ahead record is on
    /// disk before the relayer is contacted; the process then dies before any transaction hash is
    /// recorded. On restart the row is PENDING-with-no-hash, so:
    ///   * it is NOT settled → the position is never forfeited (the pre-fix failure), and
    ///   * it is NOT immediately re-submitted → no blind double-send on the very next tick;
    ///   * after `PENDING_UNKNOWN_TIMEOUT_MS` it is re-submitted exactly once more.
    ///
    /// **The chosen failure is a bounded, delayed DUPLICATE SUBMIT — never a forfeit.** That is
    /// safe because `redeemPositions` burns the caller's balance: a second call against an
    /// already-redeemed condition pays nothing rather than paying twice (see `redeem_ledger`'s
    /// module doc), and because the retry only fires for a position the data-api still reports as
    /// redeemable — i.e. one whose first submit did not land.
    #[test]
    fn crash_between_write_ahead_and_confirmation_prefers_a_delayed_duplicate_submit_over_a_forfeit()
     {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        // the relayer answers 2xx with NO hash — the pinned `{"state":"NEW"}` shape, and also what a
        // crash before `attach_tx` leaves behind.
        let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]).without_tx_hash();

        {
            let l = RedeemLedger::open(path.clone());
            redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
            // "crash": drop the ledger with the row still pending and no hash.
        }
        assert_eq!(deps.submits().len(), 1);

        let l = RedeemLedger::open(path);
        assert!(!l.is_settled("0x1"), "NOT a forfeit: the crashed submit is not a tombstone");
        assert_eq!(l.pending("0x1").map(|p| p.tx_hash), Some(None), "pending, no hash to confirm");

        // Inside the window: suppressed, no second send.
        let inside =
            redeem_once(&deps, "0xproxy", &l, &HashSet::new(), PENDING_UNKNOWN_TIMEOUT_MS - 1);
        assert_eq!(inside.awaiting, vec!["0x1".to_string()]);
        assert!(inside.submitted.is_empty());
        assert_eq!(deps.submits().len(), 1, "no double-send inside the window");

        // Past the window: exactly one more attempt — the deliberate duplicate-submit choice.
        let after = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), PENDING_UNKNOWN_TIMEOUT_MS);
        assert_eq!(after.submitted, vec!["0x1".to_string()]);
        assert_eq!(deps.submits().len(), 2);
        assert_eq!(l.pending("0x1").map(|p| p.attempts), Some(2));
    }

    /// The other half of the crash choice: if the crashed submit DID land, the position stops being
    /// redeemable and discovery never re-offers it — so the duplicate submit above does not even
    /// happen in the case that matters.
    #[test]
    fn a_landed_submit_is_never_retried_because_discovery_stops_offering_it() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]).without_tx_hash();
        redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);

        // the redeem landed on-chain: the data-api now reports the position as no longer redeemable.
        let settled_deps = StubDeps::new(vec![pos("0x1", false, 100.0, false)]).without_tx_hash();
        let rep = redeem_once(
            &settled_deps,
            "0xproxy",
            &l,
            &HashSet::new(),
            PENDING_UNKNOWN_TIMEOUT_MS * 10,
        );
        assert!(rep.submitted.is_empty(), "gone from discovery ⇒ never re-submitted");
        assert!(settled_deps.submits().is_empty());
    }

    /// A relayer error leaves the pending record standing (the outcome is genuinely unknown), so
    /// the retry waits out the submit window instead of firing on the next tick.
    #[test]
    fn a_relayer_error_leaves_the_row_pending_and_delays_the_retry() {
        struct FailingDeps {
            calls: Mutex<u32>,
        }
        impl RedeemDeps for FailingDeps {
            fn list_positions(&self, _p: &str) -> Result<Vec<Position>, String> {
                Ok(vec![pos("0x1", true, 100.0, false)])
            }
            fn redeem(&self, _p: &str, _c: &str, _k: &RedeemKind) -> Result<RedeemResult, String> {
                *self.calls.lock().unwrap() += 1;
                Err("relayer 500".into())
            }
            fn confirm(&self, _c: &str, _t: &str) -> Result<RedeemConfirmation, String> {
                panic!("no tx hash was ever recorded — confirm must not be called");
            }
            fn payout(&self, _p: &Position) -> Payout {
                Payout::Winner
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let deps = FailingDeps { calls: Mutex::new(0) };

        let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
        assert_eq!(rep.failed, vec!["0x1".to_string()]);
        assert!(rep.submitted.is_empty());
        assert!(!l.is_settled("0x1"), "a failed submit is never a tombstone");
        assert!(l.pending("0x1").is_some(), "left pending: accepted-then-disconnected is possible");

        // next tick, inside the window → no second POST.
        redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 1_000);
        assert_eq!(*deps.calls.lock().unwrap(), 1);
        // past the window → one more.
        redeem_once(&deps, "0xproxy", &l, &HashSet::new(), PENDING_UNKNOWN_TIMEOUT_MS);
        assert_eq!(*deps.calls.lock().unwrap(), 2);
    }

    /// Confirmation does not depend on discovery: a data-api outage still lets an in-flight
    /// redemption settle.
    #[test]
    fn confirmation_runs_even_when_the_data_api_is_down() {
        struct DiscoveryDownDeps {
            inner: StubDeps,
            down: Mutex<bool>,
        }
        impl RedeemDeps for DiscoveryDownDeps {
            fn list_positions(&self, p: &str) -> Result<Vec<Position>, String> {
                if *self.down.lock().unwrap() {
                    return Err("data-api 503".into());
                }
                self.inner.list_positions(p)
            }
            fn redeem(&self, p: &str, c: &str, k: &RedeemKind) -> Result<RedeemResult, String> {
                self.inner.redeem(p, c, k)
            }
            fn confirm(&self, c: &str, t: &str) -> Result<RedeemConfirmation, String> {
                self.inner.confirm(c, t)
            }
            fn payout(&self, p: &Position) -> Payout {
                self.inner.payout(p)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let deps = DiscoveryDownDeps {
            inner: StubDeps::new(vec![pos("0x1", true, 100.0, false)])
                .scripted(vec![settled(90_715_029)]),
            down: Mutex::new(false),
        };
        redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0); // submit while discovery works
        *deps.down.lock().unwrap() = true;
        let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 1_000);
        assert_eq!(rep.settled, vec!["0x1".to_string()], "settled despite the data-api being down");
        assert!(l.is_settled("0x1"));
    }

    /// A session-disabled conditionId still gets CONFIRMED — an exclusion must stop new submits, not
    /// strand money that is already in flight.
    #[test]
    fn an_excluded_condition_is_still_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let deps =
            StubDeps::new(vec![pos("0x1", true, 100.0, false)]).scripted(vec![settled(90_715_029)]);
        redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);

        let mut exclude = HashSet::new();
        exclude.insert("0x1".to_string());
        let rep = redeem_once(&deps, "0xproxy", &l, &exclude, 1_000);
        assert_eq!(rep.settled, vec!["0x1".to_string()]);
        assert!(l.is_settled("0x1"));
    }

    // =============================================================================================
    // THE WINNER CHECK: `redeemable: true` is a RESOLUTION flag, so only the PAYOUT may authorise a
    // relayer call. The fixture is this repo's real mainnet wallet — see `positions.rs`'s
    // `live_wallet_fixture` for the same table exercised at the filter level.
    // =============================================================================================

    /// The four real positions of funder `0x107C01D0…`, in data-api order: three resolved LOSERS
    /// then the one $5 winner, every row `redeemable: true`.
    fn live_wallet() -> Vec<Position> {
        vec![
            pos("0x13bf6efb", true, 29.11, false), // Solana Up/Down Jun 9  — LOSER
            pos("0xbf336239", true, 10.0, false),  // Doge  Up/Down Jun 11  — LOSER
            pos("0x5a71ff88", true, 10.0, false),  // Doge  Up/Down Jun 9   — LOSER
            pos("0xf361b0aa", true, 5.0, false),   // BNB   Up/Down Jun 12  — WINNER, $5
        ]
    }

    fn live_wallet_payouts() -> Vec<(&'static str, Payout)> {
        vec![
            ("0x13bf6efb", Payout::Loser),
            ("0xbf336239", Payout::Loser),
            ("0x5a71ff88", Payout::Loser),
            ("0xf361b0aa", Payout::Winner),
        ]
    }

    /// **The D1 regression guard.** All four rows report `redeemable: true`; exactly one pays. The
    /// poller must contact the relayer ONCE — for the winner — not four times.
    #[test]
    fn redeem_once_submits_only_the_winner_of_four_redeemable_positions() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let deps = StubDeps::new(live_wallet()).with_payouts(&live_wallet_payouts());

        let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
        assert_eq!(
            rep.submitted,
            vec!["0xf361b0aa".to_string()],
            "one relayer call, for the only position that pays"
        );
        assert_eq!(
            deps.submits(),
            vec![("0xf361b0aa".to_string(), false)],
            "the three resolved losers never reach submit_redeem"
        );
        for loser in ["0x13bf6efb", "0xbf336239", "0x5a71ff88"] {
            assert!(l.pending(loser).is_none(), "no ledger row is written for {loser}");
            assert!(!l.is_settled(loser), "and certainly no tombstone");
        }
    }

    /// A `Payout::Unknown` verdict (an RPC blip, or a chain that says the condition has not resolved)
    /// submits NOTHING and writes NOTHING — and the very next pass, once the source answers,
    /// submits the winner. Fail closed is a delay, never a forfeit.
    #[test]
    fn an_unknown_payout_submits_nothing_and_the_next_pass_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        let l = RedeemLedger::open(path);

        // EVERY cid unknown — an unreachable RPC does not answer for one condition and not another.
        let blind = StubDeps::new(live_wallet()).with_payouts(
            &live_wallet_payouts().iter().map(|(c, _)| (*c, Payout::Unknown)).collect::<Vec<_>>(),
        );
        let rep = redeem_once(&blind, "0xproxy", &l, &HashSet::new(), 0);
        assert!(rep.submitted.is_empty() && blind.submits().is_empty(), "nothing on an Unknown");
        assert!(l.pending("0xf361b0aa").is_none(), "and no write-ahead record either");

        let seeing = StubDeps::new(live_wallet()).with_payouts(&live_wallet_payouts());
        let rep = redeem_once(&seeing, "0xproxy", &l, &HashSet::new(), 1_000);
        assert_eq!(rep.submitted, vec!["0xf361b0aa".to_string()], "recovered on the next pass");
    }

    /// A neg-risk LOSER is skipped by the same rule — the winner check runs before the binary /
    /// neg-risk routing, so neither contract path can be handed a worthless position.
    #[test]
    fn a_neg_risk_loser_is_skipped_too() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let deps =
            StubDeps::new(vec![pos("0xneg", true, 7.0, true), pos("0xbin", true, 7.0, false)])
                .with_payouts(&[("0xneg", Payout::Loser), ("0xbin", Payout::Winner)]);
        let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
        assert_eq!(rep.submitted, vec!["0xbin".to_string()]);
        assert_eq!(deps.submits(), vec![("0xbin".to_string(), false)]);
    }

    // =============================================================================================
    // Pre-existing behaviour that must not regress.
    // =============================================================================================

    /// Both a binary AND a neg-risk winner are submitted (an unresolved position is skipped), then
    /// both settle on chain proof.
    #[test]
    fn redeem_once_submits_binary_and_neg_risk_winners_then_settles_them() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let deps = StubDeps::new(vec![
            pos("0x1", true, 100.0, false), // binary winner
            pos("0x2", false, 5.0, false),  // still trading (condition not resolved)
            pos("0x3", true, 5.0, true),    // neg-risk winner
        ])
        .scripted(vec![settled(1)]);

        let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
        assert_eq!(rep.submitted.len(), 2, "both winners submitted, the unresolved one skipped");
        assert!(rep.settled.is_empty(), "nothing settles on the submit tick");
        assert!(!l.is_settled("0x1") && !l.is_settled("0x3"));

        let rep2 = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 1_000);
        assert_eq!(rep2.settled.len(), 2, "both settle once the chain proves them");
        assert!(l.is_settled("0x1") && l.is_settled("0x3"));

        let calls = deps.submits();
        assert!(calls.contains(&("0x1".to_string(), false)), "0x1 routed Binary");
        assert!(calls.contains(&("0x3".to_string(), true)), "0x3 routed NegRisk");
    }

    /// The routing assertion: a binary winner routes [`RedeemKind::Binary`] and a neg-risk winner
    /// routes [`RedeemKind::NegRisk`] (with a non-empty amounts array — asserted in the stub).
    #[test]
    fn redeem_once_routes_binary_and_neg_risk() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false), pos("0x3", true, 5.0, true)]);
        let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
        let calls = deps.submits();
        assert!(calls.contains(&("0x1".to_string(), false)));
        assert!(calls.contains(&("0x3".to_string(), true)));
        assert_eq!(rep.submitted.len(), 2);
    }

    /// `neg_risk_amounts` puts the 6-decimal base-unit balance at the position's slot and 0 at the
    /// other, and is panic-free for garbage sizes (NaN/negative → 0). ASSUMED derivation — the exact
    /// on-chain amount correctness is the arbdub live-verify item; this just guards the shape.
    #[test]
    fn neg_risk_amounts_places_base_units_at_slot() {
        let mut p = pos("0xc", true, 5.0, true); // slot 0, 5.0 tokens
        assert_eq!(neg_risk_amounts(&p), vec![5_000_000, 0]);
        p.outcome_index = Some(1);
        assert_eq!(neg_risk_amounts(&p), vec![0, 5_000_000]);
        p.outcome_index = Some(7); // out-of-range slot clamps to 1
        assert_eq!(neg_risk_amounts(&p), vec![0, 5_000_000]);
        // panic-free garbage sizes → 0 amount (safe no-op).
        let mut bad = pos("0xc", true, f64::NAN, true);
        assert_eq!(neg_risk_amounts(&bad), vec![0, 0]);
        bad.size = -3.0;
        assert_eq!(neg_risk_amounts(&bad), vec![0, 0]);
    }

    /// An UNREADABLE slot never invents one: the amounts array stays all-zero, the same safe no-op a
    /// garbage size yields. Belt-and-braces — `payout_of` already refuses such a position, which the
    /// second half asserts end-to-end: it is not even a redeem candidate, so nothing is submitted.
    #[test]
    fn neg_risk_amounts_invents_no_slot_when_the_outcome_index_is_unreadable() {
        let mut p = pos("0xc", true, 5.0, true);
        p.outcome_index = None;
        assert_eq!(neg_risk_amounts(&p), vec![0, 0], "no slot to address ⇒ no amount placed");

        // ...and it can't get here anyway: an unreadable slot is `Unknown` under every source.
        assert_eq!(payout_of(&p, &WinnerSource::CurPriceOnly), Payout::Unknown);

        // End-to-end through `redeem_once` with the REAL payout policy (`ProdDeps`'s
        // `CurPriceOnly` arm verbatim) rather than `StubDeps`'s scripted verdict — otherwise this
        // would only be testing the stub. `p` is `redeemable: true`, `size > 0`, `curPrice: 1.0`:
        // everything the old code needed to call it a winner. Only the unreadable slot stops it.
        struct RealPayoutDeps(StubDeps);
        impl RedeemDeps for RealPayoutDeps {
            fn list_positions(&self, p: &str) -> Result<Vec<Position>, String> {
                self.0.list_positions(p)
            }
            fn redeem(&self, p: &str, c: &str, k: &RedeemKind) -> Result<RedeemResult, String> {
                self.0.redeem(p, c, k)
            }
            fn confirm(&self, c: &str, t: &str) -> Result<RedeemConfirmation, String> {
                self.0.confirm(c, t)
            }
            fn payout(&self, p: &Position) -> Payout {
                payout_of(p, &WinnerSource::CurPriceOnly)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let deps = RealPayoutDeps(StubDeps::new(vec![p]));
        let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
        assert!(rep.submitted.is_empty(), "an unreadable slot must not reach the relayer");
        assert!(deps.0.submits().is_empty(), "redeem() was never called");
        assert!(l.pending("0xc").is_none(), "and no ledger row was written");
    }

    #[test]
    fn spawn_returns_none_without_opt_in() {
        // POLY_AUTO_REDEEM unset -> never starts (default OFF), even with creds present. Driven
        // through the pure gate (`auto_redeem_enabled` is a one-line wrapper over it): edition 2024
        // made `std::env::set_var` an `unsafe fn` this workspace forbids.
        assert!(!auto_redeem_enabled_in(None), "unset ⇒ OFF");
        assert!(!auto_redeem_enabled_in(Some("")), "empty ⇒ OFF");
        assert!(!auto_redeem_enabled_in(Some("true")), "only the EXACT string \"1\" enables it");
        assert!(auto_redeem_enabled_in(Some("1")));
    }

    #[test]
    fn kill_switch_env_or_file() {
        // Over the pure predicate, with the env PRESENCE bit supplied: `std::env::set_var` is an
        // `unsafe fn` since edition 2024, which this workspace forbids, and `kill_switch_tripped`
        // is a one-line wrapper over this.
        let dir = tempfile::tempdir().unwrap();
        let halt = dir.path().join("POLY_REDEEM_HALT");
        assert!(!kill_switch_tripped_in(false, &halt), "neither env nor file set");

        std::fs::write(&halt, "").unwrap();
        assert!(kill_switch_tripped_in(false, &halt), "halt file present trips it");
        std::fs::remove_file(&halt).unwrap();
        assert!(!kill_switch_tripped_in(false, &halt), "removing the file un-trips it");

        assert!(kill_switch_tripped_in(true, &halt), "env present trips it");
        assert!(!kill_switch_tripped_in(false, &halt), "env absent again un-trips it");
    }

    /// CRITICAL real-money regression guard: a wallet holding BOTH legs (YES+NO) of one resolved
    /// binary market yields two Position rows with the SAME condition_id (different outcome_index),
    /// both redeemable. `redeemPositions` settles the whole condition once, so the poller must call
    /// `redeem` for that condition EXACTLY ONCE per tick — not once per leg.
    #[test]
    fn redeem_once_dedupes_both_legs_of_one_condition() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let yes = pos("0xcond", true, 100.0, false); // outcome_index 0 (from `pos`)
        let mut no = pos("0xcond", true, 100.0, false);
        no.outcome_index = Some(1); // the other leg of the SAME condition
        let deps = StubDeps::new(vec![yes, no]);
        let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
        assert_eq!(rep.submitted, vec!["0xcond".to_string()], "one condition submitted, not two");
        assert_eq!(
            deps.submits(),
            vec![("0xcond".to_string(), false)],
            "redeem() called EXACTLY ONCE for the shared condition (no double on-chain submit)"
        );
        assert!(l.pending("0xcond").is_some());
    }

    /// The bounded-failure policy is FUNCTIONAL: a cid in the `exclude` set never reaches
    /// `deps.redeem`. This is what the poller feeds after MAX_CONSECUTIVE_FAILURES, so a
    /// permanently-failing cid stops being retried for the rest of the session.
    #[test]
    fn redeem_once_excludes_session_disabled_cids() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]);
        let mut exclude = HashSet::new();
        exclude.insert("0x1".to_string());
        let rep = redeem_once(&deps, "0xproxy", &l, &exclude, 0);
        assert!(rep.submitted.is_empty(), "excluded cid is not submitted");
        assert!(deps.submits().is_empty(), "redeem() not called for an excluded cid");
    }

    /// End-to-end of the bounded-failure loop logic (the exact bookkeeping the poller thread runs):
    /// a cid whose redeem keeps REVERTING on chain hits the bound and stops being attempted. This
    /// is the pre-fix `cid_failing_k_times_stops_being_attempted`, re-aimed at the failure mode the
    /// fix made visible — a revert used to be invisible (it was tombstoned as a success).
    #[test]
    fn cid_reverting_k_times_stops_being_attempted() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        // every confirmation reverts → every tick re-submits, until the bound bites.
        let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]).scripted(vec![reverted()]);

        let mut consecutive_failures: HashMap<String, u32> = HashMap::new();
        let mut disabled_this_session: HashSet<String> = HashSet::new();
        let mut submits_per_tick = Vec::new();
        for tick in 0..(MAX_CONSECUTIVE_FAILURES + 4) {
            let report =
                redeem_once(&deps, "0xproxy", &l, &disabled_this_session, i64::from(tick) * 1_000);
            for cid in &report.settled {
                consecutive_failures.remove(cid);
            }
            for cid in report.failed.iter().chain(report.reverted.iter()) {
                let n = consecutive_failures.entry(cid.clone()).or_insert(0);
                *n += 1;
                if *n >= MAX_CONSECUTIVE_FAILURES {
                    disabled_this_session.insert(cid.clone());
                }
            }
            submits_per_tick.push(deps.submits().len());
        }
        assert!(disabled_this_session.contains("0x1"), "the bound bit");
        let total = *submits_per_tick.last().unwrap();
        assert_eq!(
            submits_per_tick[submits_per_tick.len() - 2],
            total,
            "no further relayer calls once the bound bit"
        );
        // K reverts disable it; tick 0's own submit precedes the first revert, so the ceiling is
        // K+1 attempts, bounded either way — the point is that it STOPS.
        assert!(
            total <= MAX_CONSECUTIVE_FAILURES as usize + 1,
            "bounded retries, got {total} relayer calls"
        );
    }

    /// An on-chain settlement clears the failure counter — the poller's own bookkeeping, mirrored.
    #[test]
    fn a_settlement_clears_the_failure_counter() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]).scripted(vec![
            reverted(),
            RedeemConfirmation::Unmined,
            settled(7),
        ]);
        let mut consecutive_failures: HashMap<String, u32> = HashMap::new();
        for tick in 0..4 {
            let report =
                redeem_once(&deps, "0xproxy", &l, &HashSet::new(), i64::from(tick) * 1_000);
            for cid in &report.settled {
                consecutive_failures.remove(cid);
            }
            for cid in report.failed.iter().chain(report.reverted.iter()) {
                *consecutive_failures.entry(cid.clone()).or_insert(0) += 1;
            }
        }
        assert!(l.is_settled("0x1"));
        assert!(consecutive_failures.is_empty(), "the settlement cleared the revert count");
    }

    #[test]
    fn spawn_returns_none_without_creds() {
        // `spawn` checks creds BEFORE the opt-in env var, so this needs no POLY_AUTO_REDEEM
        // mutation (and so can't race the other env-var tests running in parallel).
        let dir = tempfile::tempdir().unwrap();
        let h = AutoRedeemPoller::spawn(
            None,
            "0xproxy".into(),
            dir.path().join("ledger.jsonl"),
            dir.path().join("HALT"),
            Duration::from_secs(60),
        );
        assert!(h.is_none(), "no creds -> never starts");
    }
}
