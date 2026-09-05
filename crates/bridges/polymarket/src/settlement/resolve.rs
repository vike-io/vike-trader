//! `resolve` — condition-resolution watchlist + **LOCAL position settlement**. The missing half of
//! CTF auto-redeem: `auto_redeem` moves the *money* on-chain (redeem a winning position via the
//! gasless relayer), but nothing ever closed the *local book* — a resolved market's position and its
//! unrealized PnL linger in `Account` until the redeem lands (and, for a LOSING leg, forever: a loser
//! is never redeemed, so nothing would ever retire it). This module emits the terminal settlement
//! fill that flattens the local position at its payout.
//!
//! Deliberately mirrors `auto_redeem`'s shape one-for-one — same trait seam for testability
//! (`ResolveDeps` ~ `RedeemDeps`), same `*_once` reusable pass the thread calls on a timer, same
//! stop-aware/Drop-joining handle, same at-most-once ledger discipline ([`SettlementLedger`] mirrors
//! `redeem_ledger::RedeemLedger`). It shares `auto_redeem`'s discovery source
//! (`positions::PositionsClient`) and invents NO endpoint.
//!
//! **Opt-in, default OFF:** [`ResolvePoller::spawn`] returns `None` unless `VIKE_PM_RESOLVE=1`
//! ([`pm_resolve_enabled`]) AND a non-empty proxy address is supplied. Unset ⇒ no thread, no events,
//! byte-identical to before this module existed. Unlike `auto_redeem` this takes NO on-chain action
//! and moves no money — it only writes into our own core — so it carries no `POLY_REDEEM_HALT`-style
//! kill switch; stopping the handle is the off switch.
//!
//! ## ⚠ `redeemable` is a RESOLUTION flag, not a WINNER flag
//!
//! **all four** positions of this repo's mainnet account came back `redeemable: true` (2026-07-23),
//! three of them plain losers (`curPrice: 0`, `cashPnl: -100%`). Each condition had exactly ONE leg
//! held, so [`ambiguous_conditions`] — which needs 2+ redeemable legs of the SAME condition — does
//! not fire, and [`winning_tokens`] would have settled all four at 1.0, **fabricating ≈29 USDC of
//! profit** in `closed_pnls`. The chain's `payoutNumerators` agreed with the data-api's `curPrice`
//! on 4/4. So `redeemable` means "this condition RESOLVED and can be redeemed (possibly for zero)".
//!
//! ## Where the payout comes from: [`PayoutSource`] — the CHAIN by default
//!
//! Every [`ResolveDeps`] must NAME its payout source ([`ResolveDeps::payout_source`], deliberately
//! WITHOUT a default implementation — the twin of `auto_redeem`'s `RedeemDeps::payout`, and for the
//! same reason: any default would be a silent one, and the wrong silent default fabricates money).
//!
//! - [`PayoutSource::Chain`] — **the default, and what [`ResolvePoller::spawn`] wires**
//!   ([`ChainResolveDeps`] over a [`crate::chain::ChainOracle`]). A leg's payout is the CTF's own
//!   `payoutNumerators` for its `outcome_index`, read through
//!   [`crate::chain::PolygonRpc::condition_resolution`]; `redeemable` is not consulted at all.
//!   **Fail closed:** no chain verdict ⇒ the leg is not due and NOTHING is emitted this pass — a
//!   delay, never a guess. (No verdict conflates "not resolved yet" with "RPC unreachable"; at this
//!   seam they are indistinguishable, and both call for exactly that behaviour.) A resolved-but-
//!   unpriceable leg is likewise skipped, into [`SettleTickReport::skipped_unpriced`] — either
//!   because the data-api never gave us a readable `outcomeIndex` (it is `Option<u32>`; a guessed
//!   slot `0` is the PAYING slot of a `[1, 0]` resolution) or because the verdict's
//!   `payoutNumerators` has no slot for the one we hold. This source closes three gaps
//!   the flag cannot: a resolved LOSER with no redeemable row anywhere settles, an "ambiguous"
//!   condition settles correctly instead of being skipped, and a SPLIT resolution
//!   (`payoutNumerators [1,1] / 2`) settles at its true 0.5 — a payout no boolean flag can express.
//!   ⚠ Consequence, stated once: an unreachable Polygon RPC means this poller settles NOTHING. It
//!   idles rather than misbehaving, and every pass re-offers every watched leg.
//! - [`PayoutSource::RedeemableFlag`] — ⚠ the **named opt-out** ([`ProdResolveDeps`]), for a host
//!   that genuinely cannot reach Polygon RPC. It is the pre-chain heuristic described below, and on
//!   the wallet above it is WRONG on three legs out of four. Nothing in this workspace constructs
//!   it outside tests, and [`ResolvePoller`] offers no spawn that selects it. ⚠ Naming it opts out
//!   of CONSULTING the chain, not out of OBEYING it: where a deps names this source and still
//!   answers [`ResolveDeps::chain_resolution`], the verdict prices the leg, and an unpriceable leg
//!   is refused into [`SettleTickReport::skipped_unpriced`] exactly as above — the flag is the
//!   fallback only where the chain has said nothing.
//!
//! ## The `RedeemableFlag` rules (pinned fields only) — read these only if you named that source
//!
//! The resolution signal is `Position.redeemable` (field names pinned in `positions.rs` against
//! three independent sources). Per tick, from ONE `/positions` snapshot:
//!   - a `conditionId` with ANY `redeemable == true` row is **RESOLVED** ([`resolved_conditions`]);
//!   - that row's `asset` (outcome token) is the **WINNER** → payout [`WINNER_PAYOUT`] (1.0);
//!   - any OTHER watched token under the SAME `conditionId` is a **LOSER** → [`LOSER_PAYOUT`] (0.0).
//!
//! That winner rule assumes the data-api flags ONLY the winning leg — which the wallet above
//! refutes. Where a snapshot contradicts it visibly — 2+ DISTINCT tokens of one condition both
//! flagged redeemable — the winner is unidentifiable, and [`ambiguous_conditions`] makes the module
//! skip that condition rather than settle a leg at a guessed payout; a lingering position is cheap
//! and a fabricated realized PnL is not. And a wallet holding ONLY the losing leg has no redeemable
//! row anywhere, so this source cannot distinguish "resolved loser" from "still trading"; such a
//! position stays watched and unsettled rather than settled at a guessed 0.0. Both limits are
//! inherent to the flag, and are exactly what [`PayoutSource::Chain`] exists to remove.
//!
//! ## Why a bare `Event::Fill` (and no new Event variant)
//!
//! `ExecutionEngine::on_event` handles `Event::Fill` BEFORE any registry lookup: it symbol-filters,
//! dedups on `trade_id`, folds `Account::apply_fill` (position + realized PnL → `closed_pnls`), and
//! returns. No `client_order_id` registration is required — which is exactly right here, because a
//! settlement has no order behind it. The `OrderPartiallyFilled`/`OrderFilled` wraps are deliberately
//! NOT emitted: those drive the order FSM, require a registered coid, and an unregistered one would
//! be dropped and counted into the engine's `dropped_unknown_coid` audit counter. So the settlement
//! is ONE bare `Event::Fill` per settled position, and no `Event` variant was added.
//!
//! Downstream this also lands correctly in the journal: `journal_mat`'s order fold ignores
//! `Event::Fill` (its `fold_event` returns early), so a settlement creates NO phantom `exec_order`
//! row — only the `exec_fill` trade-log row, which is precisely what a settlement is.
//!
//! ## At-most-once
//!
//! Keyed per **(condition_id, token_id)**, not per condition: a wallet holding BOTH legs of one
//! resolved market settles TWO distinct local positions (the winner at 1.0, the loser at 0.0), so a
//! per-condition key would silently drop the second leg. That composite is both the
//! [`SettlementLedger`] key and the fill's `trade_id` (`resolution:<condition_id>:<token_id>`), so
//! the engine's own `seen_trade_ids` dedup is a second, independent guard against a double-settle
//! within a process lifetime, and the ledger file guards ACROSS restarts. The ledger is marked ONLY
//! after the event is accepted by the lane — a failed send is left unmarked and retried next tick
//! (`auto_redeem`'s exact rule).

use std::collections::HashSet;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_bridge_core::poller::{STOP_POLL_SLICE, StopHandle, sleep_stop_aware, spawn_poller};
use vike_exec::EventSender;
use vike_model::events::{Event, FillEvent, TradeId};
use vike_model::now_ms;

use crate::chain::{ChainOracle, ChainResolution};
use crate::positions::{Position, PositionsClient};

/// Payout of a winning outcome token at resolution: CTF conditional tokens settle 1 collateral unit
/// per winning token (USDC-collateralized, so $1.00).
pub const WINNER_PAYOUT: f64 = 1.0;
/// Payout of a losing outcome token at resolution: worthless.
pub const LOSER_PAYOUT: f64 = 0.0;

/// Default poll cadence — mirrors the auto-redeem poller's minute-scale rhythm. Resolution is a
/// human-timescale event (an oracle finalizing a market), so a tight loop buys nothing.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// `VIKE_PM_RESOLVE=1` is the opt-in gate — default OFF. The EXACT string `"1"`, not a fuzzy truthy
/// parse (the same discipline as `VIKE_RECONCILE` and `POLY_AUTO_REDEEM`).
pub fn pm_resolve_enabled() -> bool {
    pm_resolve_enabled_in(std::env::var("VIKE_PM_RESOLVE").ok().as_deref())
}

/// The PURE gate over the value as read — `Some("1")` exactly. Split out so a test can drive it
/// without `std::env::set_var`, an `unsafe fn` since edition 2024 that this workspace forbids.
fn pm_resolve_enabled_in(value: Option<&str>) -> bool {
    value == Some("1")
}

/// The composite at-most-once key: one settlement per held outcome token of a resolved condition.
/// See the module doc for why this is NOT keyed per condition alone.
pub fn settlement_key(condition_id: &str, token_id: &str) -> String {
    format!("{condition_id}:{token_id}")
}

// ---------------------------------------------------------------------------------------------
// Watchlist
// ---------------------------------------------------------------------------------------------

/// One held outcome token under watch. `token_id` is the ERC-1155 outcome token (the data-api's
/// `asset`), which is ALSO the vike `symbol` for this venue — the same id `user_ws` puts on every
/// fill — so a settlement fill lands on exactly the position the trading path built.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchEntry {
    pub token_id: String,
    pub condition_id: String,
    pub neg_risk: bool,
    /// Last-observed on-chain token balance (data-api `size`), refreshed on every upsert.
    pub qty: f64,
    /// Which outcome SLOT of the condition this token is (data-api `outcomeIndex`) — the index the
    /// chain's `payoutNumerators` vector is keyed by, and therefore the only thing that lets
    /// [`ResolveDeps::chain_resolution`]'s verdict be applied to this specific leg.
    ///
    /// `None` mirrors [`Position::outcome_index`]: the data-api omitted or garbled the field, so
    /// the chain's verdict CANNOT be applied to this leg. [`settle_once`] then leaves the entry
    /// unsettled rather than falling back to the `redeemable` heuristic — which, on a HELD LOSING
    /// leg (whose row also reports `redeemable: true`, this module's governing fact), would pay
    /// 1.0 and fabricate realized PnL.
    pub outcome_index: Option<u32>,
}

/// The held-instrument watchlist, upserted from the same `/positions` source `auto_redeem` polls.
///
/// `BTreeMap` (not `HashMap`) so iteration — and therefore the ORDER settlement fills are emitted in
/// — is deterministic, which keeps the scripted-sink tests exact. Insertion order is not meaningful
/// here (each settlement fill is independent; nothing sums across entries), so the house `IndexMap`
/// rule for f64-fold order does not apply and a std container avoids adding a dep to this crate.
#[derive(Debug, Default, Clone)]
pub struct ResolveWatchlist {
    entries: BTreeMap<String, WatchEntry>,
}

impl ResolveWatchlist {
    pub fn new() -> Self {
        ResolveWatchlist { entries: BTreeMap::new() }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, token_id: &str) -> Option<&WatchEntry> {
        self.entries.get(token_id)
    }

    /// Deterministic (token-id-ordered) iteration over the watched entries.
    pub fn iter(&self) -> impl Iterator<Item = &WatchEntry> {
        self.entries.values()
    }

    pub fn remove(&mut self, token_id: &str) -> Option<WatchEntry> {
        self.entries.remove(token_id)
    }

    /// Upsert every held (`size > 0`) position from a snapshot: a new token is inserted, a known one
    /// has its `qty`/`neg_risk`/`condition_id` refreshed. Returns the number of rows taken in.
    ///
    /// Zero/negative-size rows are NOT inserted — those are flat and belong to `prune_flat`, so a
    /// snapshot that reports a settled-and-redeemed leg as `size: 0` never resurrects it.
    pub fn upsert_from_positions(&mut self, positions: &[Position]) -> usize {
        let mut n = 0;
        for p in positions.iter().filter(|p| p.size > 0.0) {
            let e = self.entries.entry(p.asset.clone()).or_insert_with(|| WatchEntry {
                token_id: p.asset.clone(),
                condition_id: p.condition_id.clone(),
                neg_risk: p.neg_risk,
                qty: 0.0,
                outcome_index: p.outcome_index,
            });
            e.condition_id = p.condition_id.clone();
            e.neg_risk = p.neg_risk;
            e.qty = p.size;
            e.outcome_index = p.outcome_index;
            n += 1;
        }
        n
    }

    /// Prune on flat: drop every watched token the snapshot no longer reports with `size > 0` (sold,
    /// transferred, or redeemed away). Returns the pruned token_ids, token-id-ordered.
    ///
    /// Called AFTER settlement in a tick, never before: a resolved winner is still held (and still
    /// `redeemable`) right up until its redeem lands, so pruning first could drop an entry in the
    /// same pass that should have settled it.
    pub fn prune_flat(&mut self, positions: &[Position]) -> Vec<String> {
        let held: HashSet<&str> =
            positions.iter().filter(|p| p.size > 0.0).map(|p| p.asset.as_str()).collect();
        let gone: Vec<String> =
            self.entries.keys().filter(|t| !held.contains(t.as_str())).cloned().collect();
        for t in &gone {
            self.entries.remove(t);
        }
        gone
    }
}

// ---------------------------------------------------------------------------------------------
// Resolution status (derived from the pinned `redeemable` flag)
// ---------------------------------------------------------------------------------------------

/// ConditionIds the snapshot PROVES resolved: any row with `redeemable == true`. See the module doc
/// for the one case this cannot see (a loser-only wallet).
pub fn resolved_conditions(positions: &[Position]) -> BTreeSet<String> {
    positions.iter().filter(|p| p.redeemable).map(|p| p.condition_id.clone()).collect()
}

/// The winning outcome tokens in a snapshot (the `asset` of every redeemable row). A watched token of
/// a resolved condition that is NOT in this set is that condition's losing leg.
pub fn winning_tokens(positions: &[Position]) -> BTreeSet<String> {
    positions.iter().filter(|p| p.redeemable).map(|p| p.asset.clone()).collect()
}

/// ConditionIds whose snapshot is **AMBIGUOUS**: two or more DISTINCT outcome tokens of the SAME
/// condition are flagged `redeemable`. Settlement skips these entirely (see [`settle_once`]).
///
/// Scoped to [`PayoutSource::RedeemableFlag`]: under [`PayoutSource::Chain`] the payout comes from
/// `payoutNumerators`, which answers what this guard can only decline to guess at, so the guard is
/// not consulted at all there.
///
/// **Why this guard exists.** The winner rule ("`redeemable` row ⇒ that token won") rests on the
/// data-api marking ONLY the winning leg redeemable. That is the reading `auto_redeem` and
/// `positions.rs` encode (its fixture labels the non-redeemable row "losing"), but it is NOT
/// independently confirmed for a wallet holding BOTH legs — and `auto_redeem`'s own de-dupe test
/// constructs exactly that case with BOTH legs `redeemable: true`. The two readings disagree only
/// here, and the disagreement is expensive: if both legs really are flagged, [`winning_tokens`]
/// would call the loser a winner and settle it at 1.0, **fabricating a profit** in the local book
/// and in `closed_pnls`.
///
/// Neither reading can be confirmed from this sandbox (the data-api is geo-blocked — see
/// `positions.rs`), so the ambiguity is resolved the same conservative way the module treats a
/// loser-only wallet: **refuse to guess.** A condition with 2+ redeemable legs is left watched and
/// unsettled (the pre-feature status quo — the position simply lingers) and reported in
/// [`SettleTickReport::skipped_ambiguous`], rather than settled on a coin-flip. Not settling costs
/// a lingering position; settling wrong writes a false realized PnL into the user's account.
///
/// A single-leg-held resolved market — the overwhelmingly common case, and the one `auto_redeem`
/// itself is built around — is never ambiguous and settles normally under either reading.
pub fn ambiguous_conditions(positions: &[Position]) -> BTreeSet<String> {
    let mut by_condition: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for p in positions.iter().filter(|p| p.redeemable) {
        by_condition.entry(p.condition_id.as_str()).or_default().insert(p.asset.as_str());
    }
    by_condition
        .into_iter()
        .filter(|(_, tokens)| tokens.len() > 1)
        .map(|(cid, _)| cid.to_string())
        .collect()
}

/// Payout for one watched entry given the snapshot's winner set: 1.0 if this very token won, else
/// 0.0. Only ever called for an entry whose condition is already known resolved AND unambiguous.
pub fn payout_for(entry: &WatchEntry, winners: &BTreeSet<String>) -> f64 {
    if winners.contains(&entry.token_id) { WINNER_PAYOUT } else { LOSER_PAYOUT }
}

// ---------------------------------------------------------------------------------------------
// The settlement fill
// ---------------------------------------------------------------------------------------------

/// Build the terminal settlement fill that flattens `signed_qty` of `entry.token_id` at `payout`.
///
/// `signed_qty` is the LOCAL signed position being closed (positive = long). The fill is its exact
/// inverse — `side = -sign(signed_qty)`, `last_qty = |signed_qty|` — so `Account::fold` reduces the
/// position to zero and books the realized PnL of the closed portion into `closed_pnls`. A long that
/// won realizes `(1.0 - avg_px) * qty`; a long that lost realizes `-avg_px * qty`.
///
/// Field construction mirrors `user_ws::fill_pair` (the crate's existing FillEvent site) exactly,
/// with three deliberate differences: `trade_id` carries the `resolution:` tag (module doc),
/// `liquidity_side` is empty (a settlement is neither maker nor taker — the model's documented
/// "venue did not surface it"), and `mark_price` stays `None` so a settlement never writes the
/// price board (the position is flat afterwards, so there is nothing left to mark).
pub fn settlement_fill(entry: &WatchEntry, payout: f64, signed_qty: f64, ts: i64) -> FillEvent {
    let side = vike_model::closing_side(signed_qty);
    FillEvent {
        trade_id: settlement_trade_id(&entry.condition_id, &entry.token_id),
        client_order_id: format!("resolution:{}", entry.condition_id),
        venue: crate::VENUE.to_string().into(),
        symbol: entry.token_id.clone().into(),
        side,
        last_qty: signed_qty.abs(),
        last_px: payout,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        ts,
        mark_price: None,
        position_side: "BOTH".to_string().into(),
    }
}

/// The settlement fill's `trade_id` — unique per settled position, and the engine's own dedup key.
///
/// Minted by vike, so it is built with [`TradeId::prefixed`] off the static `"resolution:"` tag:
/// non-empty by construction, no fallible path, and the rendered string is BYTE-IDENTICAL to the
/// `format!` this used to return. The byte shape is a correctness requirement, not tidiness — the
/// module doc's whole dedup argument is that the resolve poller and
/// `crate::recon_client::settlement_fill_report` stamp the SAME string for the same settled
/// position, on every process, restart and replay, so one character of drift double-settles.
pub fn settlement_trade_id(condition_id: &str, token_id: &str) -> TradeId {
    TradeId::prefixed("resolution:", format_args!("{condition_id}:{token_id}"))
}

// ---------------------------------------------------------------------------------------------
// Ledger (mirrors redeem_ledger::RedeemLedger)
// ---------------------------------------------------------------------------------------------

/// Persisted set of already-settled `(condition_id, token_id)` keys — the at-most-once store across
/// process restarts. A structural mirror of `redeem_ledger::RedeemLedger` (one key per line,
/// thread-safe, best-effort persistence: a write failure warns but never kills the poller, and the
/// in-memory set still guards the running session) kept as its own type and its own FILE so the
/// redeem ledger's "redeemed conditionIds" meaning is not overloaded with a different key shape.
pub struct SettlementLedger {
    path: PathBuf,
    seen: Mutex<HashSet<String>>,
}

impl SettlementLedger {
    /// Open, loading any existing keys.
    pub fn open(path: PathBuf) -> Self {
        let mut seen = HashSet::new();
        if let Ok(txt) = std::fs::read_to_string(&path) {
            for line in txt.lines() {
                let k = line.trim();
                if !k.is_empty() {
                    seen.insert(k.to_string());
                }
            }
        }
        SettlementLedger { path, seen: Mutex::new(seen) }
    }

    pub fn contains(&self, condition_id: &str, token_id: &str) -> bool {
        self.seen.lock().unwrap().contains(&settlement_key(condition_id, token_id))
    }

    /// Record this position as settled (in-memory + append to disk). Idempotent; a persist error
    /// warns only.
    pub fn mark(&self, condition_id: &str, token_id: &str) {
        let key = settlement_key(condition_id, token_id);
        let newly = self.seen.lock().unwrap().insert(key.clone());
        if !newly {
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{key}") {
                    tracing::warn!(%key, %e, "SettlementLedger: persist failed (in-memory guard holds)");
                }
            }
            Err(e) => tracing::warn!(%key, %e, "SettlementLedger: open-for-append failed"),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The tick
// ---------------------------------------------------------------------------------------------

/// Where a [`ResolveDeps`] gets PAYOUT truth from — the resolve twin of `auto_redeem`'s
/// `WinnerSource`. See the module doc's "Where the payout comes from" section; the one-line version
/// is that [`Chain`](PayoutSource::Chain) is authoritative and fails closed, and
/// [`RedeemableFlag`](PayoutSource::RedeemableFlag) is a heuristic that is known to be wrong on a
/// real wallet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayoutSource {
    /// The CTF's own `payoutNumerators`, via [`ResolveDeps::chain_resolution`]. The DEFAULT, and the
    /// only source [`ResolvePoller`] will spawn. A leg the chain cannot price is not settled.
    Chain,
    /// ⚠ The data-api's `redeemable` flag — the pre-chain heuristic, kept only as an explicitly
    /// named opt-out for a host that cannot reach Polygon RPC. It settles EVERY leg of a resolved
    /// condition at 1.0 unless a sibling redeemable row happens to identify the winner, which on
    /// this repo's own mainnet wallet fabricates ≈29 USDC of profit.
    ///
    /// It is the fallback only where NO chain verdict exists. A deps that names this source but
    /// still answers [`ResolveDeps::chain_resolution`] is priced BY that verdict, and where the
    /// verdict cannot price the leg it refuses ([`SettleTickReport::skipped_unpriced`]) rather than
    /// falling back — naming this source opts out of consulting the chain, never out of obeying it.
    RedeemableFlag,
}

/// Test seam: everything the loop needs from the outside world — the twin of `auto_redeem`'s
/// `RedeemDeps`. [`ChainResolveDeps`] is the production wiring (real `PositionsClient` + the
/// on-chain oracle); tests inject a scripted stub so no test touches the network.
pub trait ResolveDeps {
    fn list_positions(&self, proxy: &str) -> Result<Vec<Position>, String>;

    /// **Which payout source this deps speaks for** — see [`PayoutSource`].
    ///
    /// Deliberately has NO default implementation, exactly like `auto_redeem`'s
    /// `RedeemDeps::payout`: a default would have to be either
    /// [`Chain`](PayoutSource::Chain) (silently promising an oracle an implementor may not have) or
    /// [`RedeemableFlag`](PayoutSource::RedeemableFlag) (silently re-introducing the fabrication
    /// this seam exists to remove). Every implementor states its source out loud instead.
    fn payout_source(&self) -> PayoutSource;

    /// OPTIONAL local-book override: the LOCAL signed position for `token_id`, when the caller can
    /// supply it (e.g. reading the core's `CoreSnapshot`). Default `None` ⇒ fall back to the
    /// data-api token balance, treated as a long — correct whenever the local book was built from
    /// this wallet's own trading.
    ///
    /// It exists because the two can legitimately disagree: the on-chain balance may include tokens
    /// this process never traded (bought in the Polymarket UI, transferred in), and settling THAT
    /// quantity would push the local position negative instead of flat. When the hook returns
    /// `Some(size)` the settlement closes exactly `size`; `Some(0.0)` means "locally flat" and the
    /// settlement is skipped entirely (nothing to close).
    fn local_position(&self, _token_id: &str) -> Option<f64> {
        None
    }

    /// The **on-chain resolution oracle** — the authoritative payout source under
    /// [`PayoutSource::Chain`]. Default `None`, which is correct for a
    /// [`PayoutSource::RedeemableFlag`] implementor (it has no oracle to consult) and, under
    /// [`PayoutSource::Chain`], is the FAIL-CLOSED answer: an entry whose condition the chain does
    /// not price is not due, so nothing is emitted for it this pass.
    ///
    /// When it answers with a RESOLVED verdict, [`settle_once`] settles the entry even if no
    /// `redeemable` row proved the condition resolved (the loser-only gap), prices it at
    /// `payout_for_index(entry.outcome_index)`, and never consults [`ambiguous_conditions`] (the
    /// ambiguity is only unresolvable WITHOUT chain data).
    ///
    /// A verdict alone is not enough to price a leg: [`WatchEntry::outcome_index`] chooses WHICH
    /// numerator is read, and it is an `Option`. An entry whose slot is `None`, or whose slot the
    /// verdict has no numerator for, is left unsettled and reported in
    /// [`SettleTickReport::skipped_unpriced`] rather than falling back to the heuristic — see that
    /// field's doc for the two inputs and why they are one concern.
    ///
    /// ⚠ That refusal keys off THIS hook answering, not off [`ResolveDeps::payout_source`]: an
    /// implementor that names [`PayoutSource::RedeemableFlag`] and still overrides
    /// `chain_resolution` gets the same refusal, never the flag fallback. Overriding it is a
    /// promise that the chain is the authority for every condition it speaks about.
    ///
    /// Cost note: this is consulted per watched entry per tick for entries not yet settled, and the
    /// production implementation caches only RESOLVED verdicts (an unresolved condition can resolve
    /// at any moment). At the minute-scale cadence of this poller, with a handful of open positions,
    /// that is a few `eth_call`s a minute.
    fn chain_resolution(&self, _condition_id: &str) -> Option<ChainResolution> {
        None
    }
}

/// ⚠ **The flag-only opt-out** — real data-api discovery, no local-book override, and NO chain
/// oracle, so every payout comes from the `redeemable` heuristic ([`PayoutSource::RedeemableFlag`]).
///
/// This is NOT what [`ResolvePoller::spawn`] wires (it wires [`ChainResolveDeps`]), and nothing in
/// this workspace constructs it outside tests. It exists so a host that genuinely cannot reach
/// Polygon RPC has a named, deliberate way to say so — and so the pre-chain behaviour stays pinned
/// by tests. On this repo's own mainnet wallet it settles three losers at 1.0; read
/// [`PayoutSource::RedeemableFlag`] before choosing it.
pub struct ProdResolveDeps;

impl ResolveDeps for ProdResolveDeps {
    fn list_positions(&self, proxy: &str) -> Result<Vec<Position>, String> {
        PositionsClient::list(proxy)
    }

    fn payout_source(&self) -> PayoutSource {
        PayoutSource::RedeemableFlag
    }
}

/// **The production `ResolveDeps`**: real data-api discovery PLUS the on-chain oracle that prices
/// every settlement ([`PayoutSource::Chain`]). This is what [`ResolvePoller::spawn`] builds.
///
/// Kept as its own type rather than as a field on [`ProdResolveDeps`] so that unit struct's public
/// shape is untouched and the two payout sources are two nameable types.
pub struct ChainResolveDeps {
    oracle: Arc<ChainOracle>,
}

impl ChainResolveDeps {
    pub fn new(oracle: Arc<ChainOracle>) -> Self {
        ChainResolveDeps { oracle }
    }

    /// The oracle built from the existing `POLY_CHAIN_*` knobs — the construction
    /// [`ResolvePoller::spawn`] uses. Opens no socket (the RPC client dials lazily) and adds no new
    /// setting. Deliberately NOT gated on [`crate::chain::chain_watch_enabled`]: that flag gates the
    /// log-scanning settlement WATCHER thread, not payout truth, and this poller's own gate
    /// ([`pm_resolve_enabled`]) is the one that decides whether anything runs at all.
    pub fn from_env() -> Self {
        ChainResolveDeps::new(Arc::new(ChainOracle::from_env()))
    }
}

impl ResolveDeps for ChainResolveDeps {
    fn list_positions(&self, proxy: &str) -> Result<Vec<Position>, String> {
        PositionsClient::list(proxy)
    }

    fn payout_source(&self) -> PayoutSource {
        PayoutSource::Chain
    }

    fn chain_resolution(&self, condition_id: &str) -> Option<ChainResolution> {
        self.oracle.resolution(condition_id).filter(|r| r.is_resolved())
    }
}

/// One [`settle_once`] pass's outcome. `settled`/`failed` carry token_ids (the settled unit);
/// `pruned` carries the tokens that went flat this tick. The two `skipped_*` vectors carry the
/// watched tokens deliberately left unsettled — one per REFUSAL KIND, not one per [`PayoutSource`]:
/// `skipped_unpriced` is "a chain verdict exists and cannot price THIS leg" (which fires under
/// either source — see its doc) and `skipped_ambiguous` is "the flag cannot name a winner". Each
/// arm `continue`s, so one entry lands in at most one of them, and neither is a failure.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SettleTickReport {
    pub settled: Vec<String>,
    pub failed: Vec<String>,
    pub pruned: Vec<String>,
    /// [`PayoutSource::RedeemableFlag`] only, and only for an entry with NO chain verdict: watched
    /// tokens whose condition tripped [`ambiguous_conditions`] — 2+ of its legs flagged
    /// `redeemable`, so that source cannot say which one won. An entry that HAS a verdict is priced
    /// by it, or refused into `skipped_unpriced`, before this guard is ever reached. Never populated
    /// under [`PayoutSource::Chain`], where `payoutNumerators` answers what this guard can only
    /// decline to guess at.
    pub skipped_ambiguous: Vec<String>,
    /// Of `settled`, the tokens whose payout came from the CHAIN oracle rather than from the
    /// `redeemable` heuristic — the observability hook for the rollout, empty without an oracle.
    pub chain_priced: Vec<String>,
    /// **Whenever a chain verdict is present — under EITHER source:** watched tokens whose
    /// condition the chain reported RESOLVED but whose payout for THIS leg could not be read.
    ///
    /// The refusal keys off the VERDICT, not the declared [`PayoutSource`]: a verdict outranks the
    /// `redeemable` heuristic, so a [`PayoutSource::RedeemableFlag`] deps that DOES override
    /// [`ResolveDeps::chain_resolution`] refuses here too rather than falling back — the fallback
    /// prices a held loser at 1.0. In practice this is still a [`PayoutSource::Chain`] field: the
    /// in-tree flag-source deps ([`ProdResolveDeps`]) has no oracle, so it never yields a verdict.
    ///
    /// Two inputs land here and they are one concern (see [`settle_once`]'s match arm); the WARN
    /// log's `outcome_index` tells them apart:
    ///
    /// - [`WatchEntry::outcome_index`] is `None` — the data-api omitted or garbled our slot, so
    ///   there is no numerator to look up (`positions::parse_outcome_index`);
    /// - it is `Some(i)` but the verdict's `payoutNumerators` has no slot `i` — anomalous chain data.
    ///
    /// Both are left unsettled and UNMARKED (fail closed) rather than priced off the `redeemable`
    /// flag, which on a held loser would settle at 1.0. A later pass with a readable slot settles it
    /// correctly.
    ///
    /// A leg the chain gave NO verdict for is not listed here: it never became due, because "not
    /// resolved yet" and "RPC unreachable" are the same silence at this seam and the steady state is
    /// full of the former.
    pub skipped_unpriced: Vec<String>,
}

/// ONE discovery → upsert → settle → prune pass: the reusable core the poller thread calls on a
/// timer (the twin of `auto_redeem::redeem_once`).
///
/// `emit` is the event sink, returning `false` when the lane is gone — the same
/// `|e| events.blocking_send(e).is_ok()` shape the venue's user-data pump uses, which keeps this fold
/// testable against a plain `Vec` with no core thread.
///
/// Order within the tick is load-bearing:
/// 1. **upsert** first, so every settlement uses the qty from THIS snapshot rather than a stale one;
/// 2. **settle** each watched entry whose condition is resolved and whose `(condition_id, token_id)`
///    is not already in `ledger` — a settled entry leaves the watchlist immediately;
/// 3. **prune** last, so a resolved-but-still-held winner is never dropped before it settles.
///
/// A position that vanishes from the snapshot before its resolution is ever observed (redeemed
/// on-chain inside one poll interval) is pruned unsettled — inherent to polling, and the reason the
/// cadence is minute-scale rather than hour-scale.
pub fn settle_once(
    deps: &dyn ResolveDeps,
    proxy: &str,
    watchlist: &mut ResolveWatchlist,
    ledger: &SettlementLedger,
    emit: &mut dyn FnMut(Event) -> bool,
) -> SettleTickReport {
    let mut report = SettleTickReport::default();

    let positions = match deps.list_positions(proxy) {
        Ok(ps) => ps,
        Err(e) => {
            tracing::warn!(%e, "pm_resolve: list_positions failed this tick");
            return report;
        }
    };

    watchlist.upsert_from_positions(&positions);

    // Which payout source this deps speaks for — the whole fold below branches on it exactly once,
    // at the two places a payout could otherwise be guessed.
    let source = deps.payout_source();
    let resolved = resolved_conditions(&positions);
    let winners = winning_tokens(&positions);
    // Conditions whose winner cannot be identified from this snapshot — never settled on a guess.
    let ambiguous = ambiguous_conditions(&positions);

    // Collect first (deterministic, token-id-ordered) so the watchlist can be mutated below. Each
    // unsettled entry is paired with the CHAIN's verdict (`ResolveDeps::chain_resolution`).
    //
    // What makes an entry DUE depends on the source:
    //   - `Chain`: ONLY a resolved chain verdict. The `redeemable` flag cannot make anything due, so
    //     an unreachable RPC settles NOTHING — fail closed, retried next pass. This arm is also what
    //     makes a loser-only wallet settleable at all.
    //   - `RedeemableFlag`: the pre-chain rule — the snapshot's `redeemable` heuristic.
    let due: Vec<(WatchEntry, Option<ChainResolution>)> = watchlist
        .iter()
        .filter(|e| !ledger.contains(&e.condition_id, &e.token_id))
        .cloned()
        .map(|e| {
            let chain = deps.chain_resolution(&e.condition_id).filter(|r| r.is_resolved());
            (e, chain)
        })
        .filter(|(e, chain)| match source {
            PayoutSource::Chain => chain.is_some(),
            PayoutSource::RedeemableFlag => chain.is_some() || resolved.contains(&e.condition_id),
        })
        .collect();

    let ts = now_ms();
    for (entry, chain) in due {
        // The chain's payout for THIS leg. It needs BOTH halves: a resolved verdict from the oracle
        // AND a readable slot of our own — `outcome_index` is an `Option` because the data-api can
        // omit or garble it, and a guessed slot `0` is exactly the defect `parse_outcome_index`
        // closed (on the live `[1, 0]` Solana row, slot 0 is the PAYING one). When it answers it
        // outranks everything derived from `redeemable` — see the module doc for the live
        // refutation of that heuristic.
        let chain_payout =
            chain.as_ref().zip(entry.outcome_index).and_then(|(r, i)| r.payout_for_index(i));
        let payout = match (chain_payout, chain.is_some(), source) {
            (Some(p), _, _) => p,
            // ⚠ **A chain verdict EXISTS and could not price this leg** — and the refusal is
            // deliberately keyed on the VERDICT, not on `source`. The rule is "a chain verdict
            // exists ⇒ never fall back to the heuristic", so it holds for a
            // [`PayoutSource::RedeemableFlag`] deps that overrides [`ResolveDeps::chain_resolution`]
            // too: the alternative is `payout_for`, and a held LOSING leg's row also reports
            // `redeemable: true` (the module's governing fact), so the heuristic would put it in
            // `winners` and settle it at 1.0 — fabricating realized PnL on a build that HAS an
            // oracle, the one configuration that exists to prevent exactly that. (Narrowing this
            // onto the `Chain` arm alone was #975's regression over #974's unconditional guard.)
            //
            // Exactly two inputs reach here, and they are ONE concern — a RESOLVED verdict that
            // cannot be read against THIS leg — so they share one report field and are told apart by
            // the logged `outcome_index`, not by a second vector the caller would have to check:
            //   * `None`             — the data-api omitted or garbled our slot, so there is no
            //                          numerator to look up (`positions::parse_outcome_index`);
            //   * `Some(i)` in vain  — the verdict's `payoutNumerators` has no slot `i`.
            // The leg is left watched and unmarked — a delay, never a guessed payout.
            (None, true, _) => {
                tracing::warn!(
                    condition_id = %entry.condition_id,
                    token_id = %entry.token_id,
                    outcome_index = ?entry.outcome_index,
                    ?source,
                    "pm_resolve: chain reported the condition resolved but cannot price this leg — leaving unsettled"
                );
                report.skipped_unpriced.push(entry.token_id.clone());
                continue;
            }
            // No verdict at all, under the CHAIN source. Unreachable today — that source's `due`
            // filter admits only entries WITH a resolved verdict — and deliberately NOT
            // `unreachable!()`: this is a poller thread on a real-money book, and if that filter
            // ever widens the fail-closed skip is still the right answer, never `payout_for`.
            // Reported nowhere, exactly like the legs the filter itself declines: "no verdict" never
            // became due (see [`SettleTickReport::skipped_unpriced`]'s closing paragraph).
            (None, false, PayoutSource::Chain) => continue,
            // No verdict for this condition, so the flag heuristic is all this source has. The guard
            // above already took every entry that HAS one — note it was never about `payout_for`
            // reading `outcome_index` (it does not; it prices from the `winners` TOKEN set), but
            // about not overriding an authority that has spoken. The only refusal left here is the
            // ambiguity guard.
            (None, false, PayoutSource::RedeemableFlag) => {
                if ambiguous.contains(&entry.condition_id) {
                    // Two legs of one condition both flagged redeemable ⇒ the winner is
                    // unidentifiable. Leave it watched and unsettled (see `ambiguous_conditions` for
                    // why guessing is worse). Logged per affected watched entry, not per snapshot
                    // row, and this poller is never on the core hot fold.
                    tracing::warn!(
                        condition_id = %entry.condition_id,
                        token_id = %entry.token_id,
                        "pm_resolve: condition has multiple redeemable legs — winner unidentifiable, leaving unsettled"
                    );
                    report.skipped_ambiguous.push(entry.token_id.clone());
                    continue;
                }
                payout_for(&entry, &winners)
            }
        };
        // Local-book override when the caller supplies one; else the observed on-chain balance,
        // which is a long (CTF balances are never negative).
        let signed_qty = deps.local_position(&entry.token_id).unwrap_or(entry.qty);
        if signed_qty == 0.0 {
            // Locally flat — nothing to close. Mark it settled anyway so a permanently-flat token is
            // not re-examined every tick for the rest of the session, and drop it from the watch.
            ledger.mark(&entry.condition_id, &entry.token_id);
            watchlist.remove(&entry.token_id);
            continue;
        }
        let fill = settlement_fill(&entry, payout, signed_qty, ts);
        // Per-settlement (not per-message) — a resolution is a rare, per-order-boundary-class event,
        // and this poller is never on the core hot fold.
        tracing::info!(
            condition_id = %entry.condition_id,
            token_id = %entry.token_id,
            payout,
            qty = signed_qty,
            neg_risk = entry.neg_risk,
            source = if chain_payout.is_some() { "chain" } else { "redeemable-flag" },
            "pm_resolve: settling local position at resolution payout"
        );
        if emit(Event::Fill(fill)) {
            ledger.mark(&entry.condition_id, &entry.token_id);
            watchlist.remove(&entry.token_id);
            if chain_payout.is_some() {
                report.chain_priced.push(entry.token_id.clone());
            }
            report.settled.push(entry.token_id.clone());
        } else {
            // Lane gone: NOT ledger-marked and NOT removed from the watchlist, so the next tick
            // retries it (auto_redeem's failure rule).
            tracing::warn!(
                condition_id = %entry.condition_id,
                token_id = %entry.token_id,
                "pm_resolve: settlement emit failed — will retry next tick (not ledger-marked)"
            );
            report.failed.push(entry.token_id.clone());
        }
    }

    report.pruned = watchlist.prune_flat(&positions);
    report
}

// ---------------------------------------------------------------------------------------------
// Poller
// ---------------------------------------------------------------------------------------------

/// Owner-side handle: stop-aware shutdown of the poller thread, `Drop`-joining — the shared
/// [`StopHandle`] scaffold (`vike_bridge_core::poller`), so a dropped handle never leaks the
/// background thread (the crate's discipline — `AutoRedeemHandle`, `raw_tap.rs`).
pub type ResolveHandle = StopHandle;

pub struct ResolvePoller;

impl ResolvePoller {
    /// Spawn the resolution poller thread. Returns `None` (never starts anything) unless a non-empty
    /// `proxy` address is supplied AND [`pm_resolve_enabled`] — so an unset `VIKE_PM_RESOLVE` leaves
    /// behavior byte-identical to before this module existed.
    ///
    /// The thread loop, every `interval`: one [`settle_once`] pass against [`Self::default_deps`] —
    /// [`ChainResolveDeps`], so every payout is priced by the CTF's own `payoutNumerators` and an
    /// unpriceable leg settles NOTHING — emitting settlement fills into `events` (the same lossless
    /// ingest lane the user-data pump pushes fills into). Unlike `auto_redeem` there is no
    /// bounded-failure/disable policy: the only failure mode here is the core lane being gone, and
    /// once that happens every later tick fails identically — the operator's remedy is shutting the
    /// handle, not per-key back-off.
    ///
    /// ⚠ There is deliberately NO spawn that selects [`PayoutSource::RedeemableFlag`]: settling a
    /// resolved condition off that flag books a fabricated profit on every losing leg (module doc).
    pub fn spawn(
        proxy: String,
        ledger_path: PathBuf,
        interval: Duration,
        events: EventSender,
    ) -> Option<ResolveHandle> {
        Self::spawn_with_deps(proxy, ledger_path, interval, events, Box::new(Self::default_deps()))
    }

    /// The deps [`spawn`](Self::spawn) runs on, named so its payout source is a testable fact rather
    /// than a claim in a comment. Constructing it opens no socket.
    fn default_deps() -> ChainResolveDeps {
        ChainResolveDeps::from_env()
    }

    /// [`spawn`](Self::spawn) over a CALLER-SUPPLIED oracle instead of [`ChainOracle::from_env`] —
    /// same [`PayoutSource::Chain`] behaviour, for a caller that already holds one (e.g. alongside
    /// the chain settlement watcher) and would rather share its resolution cache than open a second
    /// RPC client. Gated exactly like the plain spawn (`VIKE_PM_RESOLVE=1` + a proxy address).
    pub fn spawn_with_chain(
        proxy: String,
        ledger_path: PathBuf,
        interval: Duration,
        events: EventSender,
        oracle: Arc<ChainOracle>,
    ) -> Option<ResolveHandle> {
        Self::spawn_with_deps(
            proxy,
            ledger_path,
            interval,
            events,
            Box::new(ChainResolveDeps::new(oracle)),
        )
    }

    fn spawn_with_deps(
        proxy: String,
        ledger_path: PathBuf,
        interval: Duration,
        events: EventSender,
        deps: Box<dyn ResolveDeps + Send>,
    ) -> Option<ResolveHandle> {
        if proxy.trim().is_empty() {
            return None;
        }
        if !pm_resolve_enabled() {
            return None;
        }

        Some(spawn_poller("vike-polymarket-resolve", move |stop| {
            let deps = deps;
            let ledger = SettlementLedger::open(ledger_path);
            let mut watchlist = ResolveWatchlist::new();
            let mut emit = |e: Event| events.blocking_send(e).is_ok();

            while !stop.load(Ordering::Relaxed) {
                settle_once(deps.as_ref(), &proxy, &mut watchlist, &ledger, &mut emit);
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

    /// Scripted stub: canned snapshots (one per tick, last repeats) + an optional local-book map.
    ///
    /// Its [`PayoutSource`] follows its wiring, so every test below reads as what it is: a stub with
    /// scripted chain verdicts speaks for [`PayoutSource::Chain`] (the production shape), and one
    /// without speaks for the [`PayoutSource::RedeemableFlag`] opt-out — which is what the pre-chain
    /// tests were always exercising. `flag_source()` forces the latter even WITH verdicts scripted,
    /// which is the ONLY way to build the source/oracle pairing nothing in-tree wires — see
    /// `a_chain_verdict_refuses_under_the_flag_source_too`.
    struct StubDeps {
        snapshots: Mutex<Vec<Vec<Position>>>,
        local: Option<BTreeMap<String, f64>>,
        /// Scripted chain oracle: conditionId → verdict. Empty ⇒ `chain_resolution` answers `None`
        /// for everything (an oracle that cannot see, or a deps that has none).
        chain: BTreeMap<String, ChainResolution>,
        /// `None` ⇒ derive from `chain`; `Some(s)` ⇒ speak for `s` regardless.
        source: Option<PayoutSource>,
    }
    impl StubDeps {
        fn new(snapshots: Vec<Vec<Position>>) -> Self {
            StubDeps {
                snapshots: Mutex::new(snapshots),
                local: None,
                chain: BTreeMap::new(),
                source: None,
            }
        }
        fn with_local(snapshots: Vec<Vec<Position>>, local: BTreeMap<String, f64>) -> Self {
            StubDeps {
                snapshots: Mutex::new(snapshots),
                local: Some(local),
                chain: BTreeMap::new(),
                source: None,
            }
        }
        /// Attach a chain verdict for one condition: `numerators` over denominator 1.
        fn with_chain(mut self, cid: &str, numerators: Vec<u128>, denominator: u128) -> Self {
            self.chain.insert(
                cid.to_string(),
                ChainResolution { condition_id: cid.into(), denominator, numerators },
            );
            self
        }
        /// Speak for the `redeemable`-flag opt-out even when chain verdicts are scripted.
        fn flag_source(mut self) -> Self {
            self.source = Some(PayoutSource::RedeemableFlag);
            self
        }
    }
    impl ResolveDeps for StubDeps {
        fn list_positions(&self, _proxy: &str) -> Result<Vec<Position>, String> {
            let mut s = self.snapshots.lock().unwrap();
            if s.len() > 1 { Ok(s.remove(0)) } else { Ok(s.first().cloned().unwrap_or_default()) }
        }
        fn payout_source(&self) -> PayoutSource {
            self.source.unwrap_or(if self.chain.is_empty() {
                PayoutSource::RedeemableFlag
            } else {
                PayoutSource::Chain
            })
        }
        fn local_position(&self, token_id: &str) -> Option<f64> {
            self.local.as_ref().map(|m| m.get(token_id).copied().unwrap_or(0.0))
        }
        fn chain_resolution(&self, condition_id: &str) -> Option<ChainResolution> {
            self.chain.get(condition_id).cloned()
        }
    }

    fn pos(cid: &str, asset: &str, size: f64, redeemable: bool, neg: bool) -> Position {
        pos_idx(cid, asset, size, redeemable, neg, 0)
    }

    fn pos_idx(
        cid: &str,
        asset: &str,
        size: f64,
        redeemable: bool,
        neg: bool,
        outcome_index: u32,
    ) -> Position {
        Position {
            condition_id: cid.into(),
            asset: asset.into(),
            size,
            redeemable,
            neg_risk: neg,
            outcome_index: Some(outcome_index),
            title: "t".into(),
            cur_price: None,
        }
    }

    /// Collecting sink: records every emitted event, always accepting.
    fn sink(out: &mut Vec<Event>) -> impl FnMut(Event) -> bool + '_ {
        move |e| {
            out.push(e);
            true
        }
    }

    fn fills(events: &[Event]) -> Vec<&FillEvent> {
        events
            .iter()
            .map(|e| match e {
                Event::Fill(f) => f,
                other => panic!("expected only bare Fill events, got {other:?}"),
            })
            .collect()
    }

    // --- fixture parsing: resolved / unresolved -------------------------------------------------

    /// A snapshot with a redeemable row proves its condition resolved and names the winning token;
    /// a snapshot with none proves nothing.
    #[test]
    fn resolution_status_from_positions_fixture() {
        let unresolved =
            vec![pos("0xA", "tokA1", 10.0, false, false), pos("0xA", "tokA2", 5.0, false, false)];
        assert!(resolved_conditions(&unresolved).is_empty(), "no redeemable row ⇒ not resolved");
        assert!(winning_tokens(&unresolved).is_empty());

        let resolved_snap =
            vec![pos("0xA", "tokA1", 10.0, true, false), pos("0xA", "tokA2", 5.0, false, false)];
        let rc = resolved_conditions(&resolved_snap);
        assert_eq!(rc.len(), 1);
        assert!(rc.contains("0xA"));
        let w = winning_tokens(&resolved_snap);
        assert!(w.contains("tokA1") && !w.contains("tokA2"));
    }

    /// The real fixture shape: `parse_positions` output feeds the resolution derivation unchanged
    /// (the JSON field pinning lives in positions.rs; this proves the seam holds end-to-end).
    #[test]
    fn parses_data_api_fixture_into_resolution_status() {
        let json = serde_json::json!([
            {"conditionId":"0xA","asset":"tokWin","size":100.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":0,"title":"won"},
            {"conditionId":"0xA","asset":"tokLose","size":40.0,"redeemable":false,"negativeRisk":false,"outcomeIndex":1,"title":"lost"},
            {"conditionId":"0xB","asset":"tokOpen","size":7.0,"redeemable":false,"negativeRisk":true,"outcomeIndex":0,"title":"still trading"}
        ]);
        let ps = crate::positions::parse_positions(&json);
        let rc = resolved_conditions(&ps);
        assert!(rc.contains("0xA"), "0xA has a redeemable leg ⇒ resolved");
        assert!(!rc.contains("0xB"), "0xB has none ⇒ left alone");
        assert_eq!(winning_tokens(&ps).iter().cloned().collect::<Vec<_>>(), vec!["tokWin"]);
    }

    #[test]
    fn payout_is_one_for_winner_zero_for_loser() {
        let winners: BTreeSet<String> = ["tokWin".to_string()].into_iter().collect();
        let win = WatchEntry {
            token_id: "tokWin".into(),
            condition_id: "0xA".into(),
            neg_risk: false,
            qty: 10.0,
            outcome_index: Some(0),
        };
        let lose = WatchEntry { token_id: "tokLose".into(), ..win.clone() };
        assert_eq!(payout_for(&win, &winners).to_bits(), WINNER_PAYOUT.to_bits());
        assert_eq!(payout_for(&lose, &winners).to_bits(), LOSER_PAYOUT.to_bits());
    }

    // --- the ambiguity guard --------------------------------------------------------------------

    /// Only a condition with 2+ DISTINCT redeemable tokens is ambiguous. One redeemable leg (the
    /// assumed-normal shape) is not, and the same token appearing twice is not either.
    #[test]
    fn ambiguous_conditions_needs_two_distinct_redeemable_tokens() {
        // normal: one winner + one loser under 0xA ⇒ unambiguous.
        let normal = vec![
            pos("0xA", "tokWin", 100.0, true, false),
            pos("0xA", "tokLose", 40.0, false, false),
        ];
        assert!(ambiguous_conditions(&normal).is_empty());

        // contradiction: BOTH legs of 0xA flagged redeemable ⇒ ambiguous.
        let both = vec![
            pos("0xA", "tokWin", 100.0, true, false),
            pos("0xA", "tokLose", 40.0, true, false),
        ];
        let amb = ambiguous_conditions(&both);
        assert_eq!(amb.len(), 1);
        assert!(amb.contains("0xA"));

        // a duplicate row for the SAME token is not a second leg.
        let dup = vec![
            pos("0xA", "tokWin", 100.0, true, false),
            pos("0xA", "tokWin", 100.0, true, false),
        ];
        assert!(ambiguous_conditions(&dup).is_empty(), "same token twice is not ambiguous");

        // distinct conditions each with their own single winner ⇒ neither is ambiguous.
        let two_markets =
            vec![pos("0xA", "tokA", 1.0, true, false), pos("0xB", "tokB", 1.0, true, false)];
        assert!(ambiguous_conditions(&two_markets).is_empty());
    }

    /// The money-protecting behavior: when both legs are flagged redeemable, NOTHING settles — no
    /// fill is emitted, nothing is ledger-marked, and both legs stay watched. This is the guard
    /// against booking a fabricated 1.0 payout on a losing leg.
    #[test]
    fn ambiguous_condition_settles_nothing_and_stays_watched() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let deps = StubDeps::new(vec![vec![
            pos("0xA", "tokWin", 100.0, true, false),
            pos("0xA", "tokLose", 40.0, true, false), // contradicts the winner rule
        ]]);
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

        assert!(out.is_empty(), "an ambiguous condition must emit NO settlement fill");
        assert!(rep.settled.is_empty() && rep.failed.is_empty());
        assert_eq!(
            rep.skipped_ambiguous,
            vec!["tokLose".to_string(), "tokWin".to_string()],
            "both legs reported skipped (token-id ordered)"
        );
        assert_eq!(wl.len(), 2, "both legs stay watched — status quo, not a guess");
        assert!(!ledger.contains("0xA", "tokWin"), "nothing marked — a later fix can still settle");
        assert!(!ledger.contains("0xA", "tokLose"));
    }

    /// The guard is scoped per condition: an ambiguous market does not block an unrelated,
    /// unambiguous one in the same snapshot.
    #[test]
    fn ambiguity_does_not_block_other_conditions() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let deps = StubDeps::new(vec![vec![
            pos("0xA", "tokWin", 100.0, true, false),
            pos("0xA", "tokLose", 40.0, true, false), // ambiguous condition
            pos("0xB", "tokClean", 7.0, true, false), // clean single-winner condition
        ]]);
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

        assert_eq!(rep.settled, vec!["tokClean".to_string()], "the clean condition still settles");
        assert_eq!(rep.skipped_ambiguous.len(), 2);
        let fs = fills(&out);
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].symbol, "tokClean");
        assert_eq!(fs[0].last_px.to_bits(), 1.0f64.to_bits());
    }

    // --- watchlist upsert / prune ---------------------------------------------------------------

    #[test]
    fn watchlist_upserts_and_refreshes_qty() {
        let mut wl = ResolveWatchlist::new();
        assert!(wl.is_empty());
        let n = wl.upsert_from_positions(&[
            pos("0xA", "tokA", 10.0, false, false),
            pos("0xB", "tokB", 3.0, false, true),
        ]);
        assert_eq!(n, 2);
        assert_eq!(wl.len(), 2);
        assert_eq!(wl.get("tokA").unwrap().qty, 10.0);
        assert_eq!(wl.get("tokA").unwrap().condition_id, "0xA");
        assert!(wl.get("tokB").unwrap().neg_risk, "neg-risk flag carried onto the entry");

        // second snapshot: tokA grew — the SAME entry is refreshed, not duplicated.
        wl.upsert_from_positions(&[pos("0xA", "tokA", 25.0, false, false)]);
        assert_eq!(wl.len(), 2, "upsert, not insert");
        assert_eq!(wl.get("tokA").unwrap().qty, 25.0);
    }

    #[test]
    fn watchlist_ignores_zero_size_rows() {
        let mut wl = ResolveWatchlist::new();
        assert_eq!(wl.upsert_from_positions(&[pos("0xA", "tokA", 0.0, true, false)]), 0);
        assert!(wl.is_empty(), "a flat row is never watched");
    }

    #[test]
    fn watchlist_prunes_on_flat() {
        let mut wl = ResolveWatchlist::new();
        wl.upsert_from_positions(&[
            pos("0xA", "tokA", 10.0, false, false),
            pos("0xB", "tokB", 3.0, false, false),
        ]);
        // tokB gone from the snapshot (sold/transferred) → pruned; tokA stays.
        let pruned = wl.prune_flat(&[pos("0xA", "tokA", 10.0, false, false)]);
        assert_eq!(pruned, vec!["tokB".to_string()]);
        assert_eq!(wl.len(), 1);
        assert!(wl.get("tokA").is_some());

        // a size-0 row counts as flat too
        let pruned2 = wl.prune_flat(&[pos("0xA", "tokA", 0.0, false, false)]);
        assert_eq!(pruned2, vec!["tokA".to_string()]);
        assert!(wl.is_empty());
    }

    // --- settlement emission via a scripted sink ------------------------------------------------

    /// The core case: a resolved condition where the wallet holds BOTH legs settles TWO local
    /// positions — the winner at 1.0 and the loser at 0.0 — each as ONE bare closing Fill.
    #[test]
    fn settles_both_legs_at_their_payouts() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("settled.txt"));
        let deps = StubDeps::new(vec![vec![
            pos("0xA", "tokWin", 100.0, true, false),
            pos("0xA", "tokLose", 40.0, false, false),
        ]]);
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

        assert_eq!(rep.settled.len(), 2, "both legs settled");
        assert!(rep.failed.is_empty());
        let fs = fills(&out);
        assert_eq!(fs.len(), 2, "exactly one bare Fill per settled position — no FSM wraps");

        // BTreeMap ordering: "tokLose" < "tokWin"
        let lose = fs[0];
        assert_eq!(lose.symbol, "tokLose");
        assert_eq!(lose.last_px.to_bits(), 0.0f64.to_bits(), "loser settles at 0.0");
        assert_eq!(lose.last_qty, 40.0);
        assert_eq!(lose.side, -1, "closes the long");
        assert_eq!(lose.trade_id, "resolution:0xA:tokLose");
        assert_eq!(lose.client_order_id, "resolution:0xA");
        assert_eq!(lose.venue, "polymarket");
        assert_eq!(lose.commission.to_bits(), 0.0f64.to_bits());
        assert!(lose.mark_price.is_none(), "a settlement never writes the price board");

        let win = fs[1];
        assert_eq!(win.symbol, "tokWin");
        assert_eq!(win.last_px.to_bits(), 1.0f64.to_bits(), "winner settles at 1.0");
        assert_eq!(win.last_qty, 100.0);
        assert_eq!(win.side, -1);
        assert_eq!(win.trade_id, "resolution:0xA:tokWin");

        assert!(wl.is_empty(), "settled entries leave the watchlist");
        assert!(ledger.contains("0xA", "tokWin") && ledger.contains("0xA", "tokLose"));
    }

    /// An UNRESOLVED condition is watched and left completely alone — the guard against fabricating
    /// a loss on a live position.
    #[test]
    fn unresolved_conditions_emit_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let deps = StubDeps::new(vec![vec![pos("0xB", "tokOpen", 7.0, false, false)]]);
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

        assert!(rep.settled.is_empty() && out.is_empty(), "nothing settled, nothing emitted");
        assert_eq!(wl.len(), 1, "still watched");
        assert!(!ledger.contains("0xB", "tokOpen"));
    }

    /// A losing leg of a condition whose winner is NOT held stays unsettled — the documented
    /// limitation, asserted so a future change to it is a deliberate one.
    #[test]
    fn loser_only_wallet_is_not_settled_on_a_guess() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        // Only the losing leg is held: no redeemable row anywhere ⇒ indistinguishable from open.
        let deps = StubDeps::new(vec![vec![pos("0xA", "tokLose", 40.0, false, false)]]);
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        assert!(rep.settled.is_empty() && out.is_empty());
        assert_eq!(wl.len(), 1, "kept under watch rather than settled at a guessed 0.0");
    }

    /// The neg-risk winner settles identically — `neg_risk` rides on the entry (it selects the redeem
    /// CONTRACT in auto_redeem) but never changes the local payout, which is 1.0 for any winner.
    #[test]
    fn neg_risk_winner_settles_at_one() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let deps = StubDeps::new(vec![vec![pos("0xN", "tokNeg", 12.0, true, true)]]);
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        let fs = fills(&out);
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].last_px.to_bits(), 1.0f64.to_bits());
        assert_eq!(fs[0].last_qty, 12.0);
    }

    /// Settlement runs BEFORE the prune, so a resolved winner still held this tick settles rather
    /// than being dropped; and a token that went flat WITHOUT resolving is pruned unsettled.
    #[test]
    fn settles_before_pruning_and_prunes_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let mut wl = ResolveWatchlist::new();
        // tick 1: two open positions.
        let deps = StubDeps::new(vec![
            vec![
                pos("0xA", "tokWin", 100.0, false, false),
                pos("0xC", "tokSold", 5.0, false, false),
            ],
            // tick 2: 0xA resolved (still held, pre-redeem); tokSold is gone.
            vec![pos("0xA", "tokWin", 100.0, true, false)],
        ]);
        let mut out = Vec::new();
        let r1 = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        assert!(r1.settled.is_empty() && r1.pruned.is_empty());
        assert_eq!(wl.len(), 2);

        let mut out2 = Vec::new();
        let r2 = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out2));
        assert_eq!(r2.settled, vec!["tokWin".to_string()], "resolved winner settled, not pruned");
        assert_eq!(r2.pruned, vec!["tokSold".to_string()], "vanished token pruned unsettled");
        assert!(wl.is_empty());
        assert_eq!(fills(&out2).len(), 1);
        assert!(!ledger.contains("0xC", "tokSold"), "a pruned-unsettled token is never marked");
    }

    // --- idempotency ----------------------------------------------------------------------------

    /// Re-running the SAME resolved snapshot emits nothing more: the ledger is the at-most-once
    /// guard even while the position is still reported (redeem has not landed yet).
    #[test]
    fn settle_once_is_idempotent_across_ticks() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let deps = StubDeps::new(vec![vec![pos("0xA", "tokWin", 100.0, true, false)]]);
        let mut wl = ResolveWatchlist::new();

        let mut out = Vec::new();
        let r1 = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        assert_eq!(r1.settled.len(), 1);
        assert_eq!(out.len(), 1);

        // Same snapshot again — the position is still held/redeemable, so it is re-upserted into the
        // watchlist, but the ledger keeps it from settling twice.
        let mut out2 = Vec::new();
        let r2 = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out2));
        assert!(r2.settled.is_empty(), "not settled twice");
        assert!(out2.is_empty(), "no second Fill emitted");
    }

    /// RESTART idempotency: a fresh process (new watchlist, new in-memory ledger) reading the SAME
    /// ledger FILE re-discovers the still-held resolved position and does NOT re-settle it.
    #[test]
    fn restart_does_not_resettle_from_the_ledger_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settled.txt");
        let snapshot = vec![pos("0xA", "tokWin", 100.0, true, false)];

        {
            let ledger = SettlementLedger::open(path.clone());
            let deps = StubDeps::new(vec![snapshot.clone()]);
            let mut wl = ResolveWatchlist::new();
            let mut out = Vec::new();
            let r = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
            assert_eq!(r.settled, vec!["tokWin".to_string()]);
        }
        // --- process restart: everything in-memory is gone, only the file survives ---
        let ledger2 = SettlementLedger::open(path);
        assert!(ledger2.contains("0xA", "tokWin"), "loaded from disk");
        let deps2 = StubDeps::new(vec![snapshot]);
        let mut wl2 = ResolveWatchlist::new();
        let mut out2 = Vec::new();
        let r2 = settle_once(&deps2, "0xproxy", &mut wl2, &ledger2, &mut sink(&mut out2));
        assert!(r2.settled.is_empty(), "restart must not re-settle");
        assert!(out2.is_empty(), "no duplicate settlement Fill after restart");
    }

    /// A failed emit (core lane gone) leaves the position UNMARKED and still watched, so the next
    /// tick retries it — `auto_redeem`'s failure rule.
    #[test]
    fn failed_emit_is_not_marked_and_retries() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let deps = StubDeps::new(vec![vec![pos("0xA", "tokWin", 100.0, true, false)]]);
        let mut wl = ResolveWatchlist::new();

        let mut rejected = 0;
        let mut reject = |_e: Event| {
            rejected += 1;
            false
        };
        let r = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut reject);
        assert_eq!(r.failed, vec!["tokWin".to_string()]);
        assert!(r.settled.is_empty());
        assert_eq!(rejected, 1);
        assert!(!ledger.contains("0xA", "tokWin"), "a failed settlement must not be marked");
        assert_eq!(wl.len(), 1, "still watched — retried next tick");

        // next tick, the lane is back: it settles.
        let mut out = Vec::new();
        let r2 = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        assert_eq!(r2.settled, vec!["tokWin".to_string()]);
        assert_eq!(out.len(), 1);
    }

    // --- local-book override --------------------------------------------------------------------

    /// With a local-position hook the settlement closes the LOCAL size, not the on-chain balance —
    /// the case where the wallet holds tokens this process never traded.
    #[test]
    fn local_position_hook_overrides_the_on_chain_qty() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let local: BTreeMap<String, f64> = [("tokWin".to_string(), 30.0)].into_iter().collect();
        // on-chain balance 100 (80 bought in the UI), locally only 30 was traded here.
        let deps =
            StubDeps::with_local(vec![vec![pos("0xA", "tokWin", 100.0, true, false)]], local);
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        let fs = fills(&out);
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].last_qty, 30.0, "closes the LOCAL size, not the 100 on-chain balance");
        assert_eq!(fs[0].side, -1);
    }

    /// A locally-flat token emits nothing (there is no position to close) but IS marked so it stops
    /// being re-examined.
    #[test]
    fn locally_flat_position_emits_nothing_but_is_marked() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let deps = StubDeps::with_local(
            vec![vec![pos("0xA", "tokWin", 100.0, true, false)]],
            BTreeMap::new(), // hook returns Some(0.0) for every token
        );
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let r = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        assert!(out.is_empty(), "nothing to close ⇒ no fill");
        assert!(r.settled.is_empty() && r.failed.is_empty());
        assert!(ledger.contains("0xA", "tokWin"), "marked so it is not re-examined every tick");
    }

    /// A local SHORT closes with a BUY — the fill is always the exact inverse of the local position.
    #[test]
    fn short_local_position_closes_with_a_buy() {
        let entry = WatchEntry {
            token_id: "tok".into(),
            condition_id: "0xA".into(),
            neg_risk: false,
            qty: 10.0,
            outcome_index: Some(0),
        };
        let f = settlement_fill(&entry, WINNER_PAYOUT, -10.0, 1_700_000_000_000);
        assert_eq!(f.side, 1, "closing a short is a BUY");
        assert_eq!(f.last_qty, 10.0, "qty is always positive");
        assert_eq!(f.ts, 1_700_000_000_000);
    }

    // --- ledger ---------------------------------------------------------------------------------

    #[test]
    fn ledger_marks_persist_and_are_keyed_per_condition_and_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settled.txt");
        let l = SettlementLedger::open(path.clone());
        assert!(!l.contains("0xA", "tokWin"));
        l.mark("0xA", "tokWin");
        assert!(l.contains("0xA", "tokWin"));
        // the OTHER leg of the SAME condition is a DIFFERENT key — the whole reason for the composite
        assert!(!l.contains("0xA", "tokLose"), "sibling leg must still be settleable");
        l.mark("0xA", "tokWin"); // idempotent
        l.mark("0xA", "tokLose");

        let l2 = SettlementLedger::open(path);
        assert!(l2.contains("0xA", "tokWin") && l2.contains("0xA", "tokLose"));
        assert!(!l2.contains("0xB", "tokWin"));
    }

    #[test]
    fn settlement_key_and_trade_id_shapes() {
        assert_eq!(settlement_key("0xA", "tok"), "0xA:tok");
        assert_eq!(settlement_trade_id("0xA", "tok"), "resolution:0xA:tok");
    }

    // --- gating ---------------------------------------------------------------------------------

    #[test]
    fn spawn_returns_none_without_a_proxy_address() {
        // The proxy check runs BEFORE the env gate, so this needs no VIKE_PM_RESOLVE mutation and
        // cannot race the env-var test below (auto_redeem's `spawn_returns_none_without_creds`
        // idiom).
        let dir = tempfile::tempdir().unwrap();
        let (events, _rx) = vike_exec::lanes::event_channel(8);
        let h = ResolvePoller::spawn(
            "   ".into(),
            dir.path().join("settled.txt"),
            Duration::from_secs(60),
            events,
        );
        assert!(h.is_none(), "no proxy address -> never starts");
    }

    #[test]
    fn resolve_is_off_by_default() {
        // Over the pure gate (`pm_resolve_enabled` is a one-line wrapper over it): edition 2024
        // made `std::env::set_var` an `unsafe fn` this workspace forbids.
        assert!(!pm_resolve_enabled_in(None), "unset ⇒ OFF");
        assert!(!pm_resolve_enabled_in(Some("true")), "only the EXACT string \"1\" enables it");
        assert!(pm_resolve_enabled_in(Some("1")));
    }

    /// A tick whose discovery fails is a no-op: nothing settled, nothing pruned, watchlist intact.
    #[test]
    fn discovery_failure_is_a_no_op_tick() {
        struct FailingDeps;
        impl ResolveDeps for FailingDeps {
            fn list_positions(&self, _proxy: &str) -> Result<Vec<Position>, String> {
                Err("data-api 503".into())
            }
            fn payout_source(&self) -> PayoutSource {
                PayoutSource::Chain
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let mut wl = ResolveWatchlist::new();
        wl.upsert_from_positions(&[pos("0xA", "tokA", 10.0, false, false)]);
        let mut out = Vec::new();
        let r = settle_once(&FailingDeps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        assert_eq!(r, SettleTickReport::default());
        assert!(out.is_empty());
        assert_eq!(wl.len(), 1, "a failed fetch must NOT prune the watchlist");
    }

    // --- the on-chain oracle seam ----------------------------------------------------------------

    /// The live four-position wallet: `(conditionId, token, size, heldOutcomeIndex)` — every row
    /// `redeemable: true` (2026-07-23), three of them plain losers, each condition holding exactly
    /// ONE leg so [`ambiguous_conditions`] can never fire on it.
    fn live_wallet() -> Vec<Position> {
        vec![
            pos_idx("0x13bf", "tokSol", 11.11, true, false, 1), // numerators [1,0] ⇒ LOSER
            pos_idx("0xbf33", "tokDoge1", 10.0, true, false, 0), // [0,1] ⇒ LOSER
            pos_idx("0x5a71", "tokDoge2", 8.0, true, false, 0), // [0,1] ⇒ LOSER
            pos_idx("0xf361", "tokBnb", 5.0, true, false, 1),   // [0,1] ⇒ WINNER
        ]
    }

    /// The chain's verdict on each of those four conditions.
    fn with_live_wallet_chain(deps: StubDeps) -> StubDeps {
        deps.with_chain("0x13bf", vec![1, 0], 1)
            .with_chain("0xbf33", vec![0, 1], 1)
            .with_chain("0x5a71", vec![0, 1], 1)
            .with_chain("0xf361", vec![0, 1], 1)
    }

    /// **The live-refuted case.** Each condition holds ONE leg, so the ambiguity guard cannot fire,
    /// and under the `redeemable`-flag source every one settles at 1.0: a fabricated profit. This
    /// test pins both halves of that: the flag-only fold is wrong, and the chain fold is right.
    #[test]
    fn chain_payouts_override_the_redeemable_flag_on_the_live_four_position_shape() {
        // (a) The `RedeemableFlag` opt-out: every leg settles at 1.0 — the fabricated profit, kept
        // pinned so choosing that source is choosing a known-wrong answer, not a surprise.
        {
            let dir = tempfile::tempdir().unwrap();
            let ledger = SettlementLedger::open(dir.path().join("s.txt"));
            let deps = StubDeps::new(vec![live_wallet()]);
            assert_eq!(deps.payout_source(), PayoutSource::RedeemableFlag);
            let mut wl = ResolveWatchlist::new();
            let mut out = Vec::new();
            let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
            assert_eq!(rep.settled.len(), 4);
            assert!(rep.chain_priced.is_empty(), "no oracle ⇒ nothing chain-priced");
            for f in fills(&out) {
                assert_eq!(f.last_px.to_bits(), 1.0f64.to_bits(), "{} settled at 1.0", f.symbol);
            }
        }

        // (b) WITH the chain: three losers at 0.0, the BNB winner at 1.0.
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let deps = with_live_wallet_chain(StubDeps::new(vec![live_wallet()]));
        assert_eq!(deps.payout_source(), PayoutSource::Chain);
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        assert_eq!(rep.settled.len(), 4);
        assert_eq!(rep.chain_priced.len(), 4, "every payout came from the chain");
        let px: BTreeMap<&str, u64> =
            fills(&out).iter().map(|f| (f.symbol.as_str(), f.last_px.to_bits())).collect();
        assert_eq!(px["tokSol"], 0.0f64.to_bits());
        assert_eq!(px["tokDoge1"], 0.0f64.to_bits());
        assert_eq!(px["tokDoge2"], 0.0f64.to_bits());
        assert_eq!(px["tokBnb"], 1.0f64.to_bits(), "the only real winner");
    }

    // --- the DEFAULT source is the chain, and it fails closed ------------------------------------

    /// **The regression guard for the poller's DEFAULT wiring.** `ResolvePoller::spawn` runs
    /// [`ChainResolveDeps`], so a pass over the live wallet with the chain UNREACHABLE (the oracle
    /// answers `None` for every condition, exactly as an RPC blackout looks at this seam) must book
    /// NOTHING — and in particular must not book any of the four at 1.0 off the `redeemable` flag.
    /// Everything stays watched and unmarked, so the next pass retries.
    #[test]
    fn chain_source_books_nothing_when_the_chain_cannot_answer() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        // `Chain` source with an EMPTY verdict map = the RPC answered nothing this pass.
        let deps = StubDeps {
            snapshots: Mutex::new(vec![live_wallet()]),
            local: None,
            chain: BTreeMap::new(),
            source: Some(PayoutSource::Chain),
        };
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

        assert!(out.is_empty(), "a silent chain must emit NO settlement fill at all");
        assert_eq!(rep, SettleTickReport::default(), "nothing settled, skipped, failed or pruned");
        assert_eq!(wl.len(), 4, "all four stay watched");
        for (cid, tok) in [
            ("0x13bf", "tokSol"),
            ("0xbf33", "tokDoge1"),
            ("0x5a71", "tokDoge2"),
            ("0xf361", "tokBnb"),
        ] {
            assert!(!ledger.contains(cid, tok), "{tok} must not be marked — the next pass retries");
        }

        // Next pass, the RPC is back: the same wallet settles at its TRUE payouts, three at 0.0.
        let deps = with_live_wallet_chain(StubDeps::new(vec![live_wallet()]));
        let mut out2 = Vec::new();
        let rep2 = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out2));
        assert_eq!(rep2.settled.len(), 4, "fail closed is a delay, never a forfeit");
        let px: BTreeMap<&str, u64> =
            fills(&out2).iter().map(|f| (f.symbol.as_str(), f.last_px.to_bits())).collect();
        assert_eq!(px["tokBnb"], 1.0f64.to_bits(), "only the real winner pays");
        for loser in ["tokSol", "tokDoge1", "tokDoge2"] {
            assert_eq!(px[loser], 0.0f64.to_bits(), "{loser} settles at 0.0, not 1.0");
        }
    }

    /// The `redeemable` flag cannot make ANYTHING due under [`PayoutSource::Chain`] — not even a
    /// condition the chain has never heard of. The partial case: two of four priced, two silent ⇒
    /// exactly two settle, and the two the chain could not price are untouched.
    #[test]
    fn chain_source_settles_only_the_legs_the_chain_priced() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let deps = StubDeps::new(vec![live_wallet()])
            .with_chain("0x13bf", vec![1, 0], 1) // LOSER, priced
            .with_chain("0xf361", vec![0, 1], 1); // WINNER, priced
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

        assert_eq!(rep.settled, vec!["tokBnb".to_string(), "tokSol".to_string()]);
        assert_eq!(rep.chain_priced.len(), 2);
        let px: BTreeMap<&str, u64> =
            fills(&out).iter().map(|f| (f.symbol.as_str(), f.last_px.to_bits())).collect();
        assert_eq!(px["tokSol"], 0.0f64.to_bits());
        assert_eq!(px["tokBnb"], 1.0f64.to_bits());
        assert_eq!(wl.len(), 2, "the two unpriced legs stay watched");
        assert!(!ledger.contains("0xbf33", "tokDoge1") && !ledger.contains("0x5a71", "tokDoge2"));
    }

    /// A chain verdict that says RESOLVED but has no slot for our `outcome_index` is anomalous data,
    /// not a licence to fall back to the flag: the leg is reported `skipped_unpriced` and left
    /// watched. (The same fixture under the flag source would settle it at 1.0.)
    #[test]
    fn chain_source_skips_a_resolved_condition_it_cannot_price_this_leg_of() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        // Held leg is slot 3; the verdict only has slots 0 and 1.
        let deps = StubDeps::new(vec![vec![pos_idx("0xA", "tokOdd", 9.0, true, false, 3)]])
            .with_chain("0xA", vec![1, 0], 1);
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

        assert!(out.is_empty(), "no fill for a leg the authority cannot price");
        assert_eq!(rep.skipped_unpriced, vec!["tokOdd".to_string()]);
        assert!(rep.settled.is_empty() && rep.chain_priced.is_empty());
        assert_eq!(wl.len(), 1, "still watched");
        assert!(!ledger.contains("0xA", "tokOdd"), "not marked — a later verdict can still settle");

        // The contrast, and the ONLY shape that still settles this fixture at 1.0: the flag opt-out
        // with no oracle at all ([`ProdResolveDeps`]'s shape), where nothing has spoken for the
        // condition but the flag. A flag-source deps that DOES answer `chain_resolution` refuses
        // exactly like the arm above — `a_chain_verdict_refuses_under_the_flag_source_too`.
        let flag_deps = StubDeps::new(vec![vec![pos_idx("0xA", "tokOdd", 9.0, true, false, 3)]]);
        assert_eq!(flag_deps.payout_source(), PayoutSource::RedeemableFlag);
        let dir2 = tempfile::tempdir().unwrap();
        let ledger2 = SettlementLedger::open(dir2.path().join("s.txt"));
        let mut wl2 = ResolveWatchlist::new();
        let mut out2 = Vec::new();
        settle_once(&flag_deps, "0xproxy", &mut wl2, &ledger2, &mut sink(&mut out2));
        assert_eq!(fills(&out2)[0].last_px.to_bits(), 1.0f64.to_bits());
    }

    /// **The refusal is unconditional in the SOURCE** — the regression this test exists for.
    ///
    /// #974 placed "a chain verdict exists ⇒ never fall back to the heuristic" BEFORE any source
    /// branch. #975 restructured into `match (chain_payout, source)`, which narrowed it onto the
    /// [`PayoutSource::Chain`] arm alone — so a deps that names [`PayoutSource::RedeemableFlag`]
    /// AND overrides [`ResolveDeps::chain_resolution`] fell through to `payout_for`, and a held
    /// LOSING leg whose row reports `redeemable: true` (the module's governing fact) settled at
    /// **1.0**, fabricating realized PnL. Nothing in-tree pairs those two — `ResolvePoller::spawn`
    /// wires [`ChainResolveDeps`] and [`ProdResolveDeps`] has no oracle — but both the trait and
    /// the enum are `pub` at the crate root, so the pairing is constructible by any caller.
    ///
    /// Both unpriceable inputs under that pairing, plus the positive control that proves the leg is
    /// priced by the VERDICT and not by the flag when the slot IS readable.
    #[test]
    fn a_chain_verdict_refuses_under_the_flag_source_too() {
        let mut unreadable = pos_idx("0xA", "tok", 10.0, true, false, 1);
        unreadable.outcome_index = None; // the data-api omitted or garbled our slot
        let out_of_range = pos_idx("0xA", "tok", 10.0, true, false, 3); // no slot 3 in `[1, 0]`

        for (label, held) in [("unreadable", unreadable), ("out-of-range", out_of_range)] {
            let dir = tempfile::tempdir().unwrap();
            let ledger = SettlementLedger::open(dir.path().join("s.txt"));
            // ⚠ The dangerous pairing: `RedeemableFlag` declared, oracle answering anyway.
            let deps =
                StubDeps::new(vec![vec![held]]).with_chain("0xA", vec![1, 0], 1).flag_source();
            assert_eq!(deps.payout_source(), PayoutSource::RedeemableFlag, "{label}");
            let mut wl = ResolveWatchlist::new();
            let mut out = Vec::new();
            let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

            assert!(out.is_empty(), "{label}: a verdict exists ⇒ NOTHING may settle off the flag");
            assert_eq!(rep.skipped_unpriced, vec!["tok".to_string()], "{label}: refused, reported");
            assert!(rep.skipped_ambiguous.is_empty(), "{label}: not the flag's ambiguity guard");
            assert!(
                rep.settled.is_empty() && rep.chain_priced.is_empty(),
                "{label}: nothing booked"
            );
            assert!(!ledger.contains("0xA", "tok"), "{label}: unmarked, so a later pass retries");
            assert_eq!(wl.len(), 1, "{label}: still watched");
        }

        // The control: same pairing, same `[1, 0]` verdict, but our slot IS readable — the leg is
        // priced by the CHAIN at 0.0, not by the `redeemable: true` flag at 1.0.
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let deps = StubDeps::new(vec![vec![pos_idx("0xA", "tokLose", 10.0, true, false, 1)]])
            .with_chain("0xA", vec![1, 0], 1)
            .flag_source();
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        assert_eq!(rep.settled, vec!["tokLose".to_string()]);
        assert_eq!(rep.chain_priced, vec!["tokLose".to_string()], "the verdict priced it");
        assert_eq!(
            fills(&out)[0].last_px.to_bits(),
            0.0f64.to_bits(),
            "0.0 from the chain, not 1.0"
        );
    }

    /// The deps `ResolvePoller::spawn` actually builds speak for the CHAIN — the asymmetry this
    /// module used to have with `auto_redeem` (which defaults to the chain) pinned shut. Building
    /// them opens no socket.
    #[test]
    fn the_default_poller_deps_are_chain_sourced() {
        assert_eq!(ResolvePoller::default_deps().payout_source(), PayoutSource::Chain);
        assert_eq!(ChainResolveDeps::from_env().payout_source(), PayoutSource::Chain);
        // And the flag-only deps still names itself honestly.
        assert_eq!(ProdResolveDeps.payout_source(), PayoutSource::RedeemableFlag);
    }

    /// The loser-only wallet — unsettleable from `/positions` alone (no redeemable row anywhere) —
    /// settles at 0.0 once the chain confirms the condition resolved against it. The complement of
    /// `loser_only_wallet_is_not_settled_on_a_guess`: with proof it is no longer a guess.
    #[test]
    fn chain_settles_a_loser_only_wallet_that_no_redeemable_row_can_reach() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        // Held leg is index 1; the chain says index 0 won ⇒ this position is worthless.
        let deps = StubDeps::new(vec![vec![pos_idx("0xA", "tokLose", 40.0, false, false, 1)]])
            .with_chain("0xA", vec![1, 0], 1);
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        assert_eq!(rep.settled, vec!["tokLose".to_string()]);
        assert_eq!(rep.chain_priced, vec!["tokLose".to_string()]);
        let fs = fills(&out);
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].last_px.to_bits(), 0.0f64.to_bits());
        assert_eq!(fs[0].last_qty, 40.0);
        assert!(wl.is_empty());
    }

    /// An UNRESOLVED condition is still left alone with a chain oracle wired: the oracle's
    /// `denominator == 0` verdict is filtered out before it can make anything due.
    #[test]
    fn chain_oracle_does_not_settle_an_unresolved_condition() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let deps = StubDeps::new(vec![vec![pos("0xB", "tokOpen", 7.0, false, false)]]).with_chain(
            "0xB",
            vec![],
            0,
        );
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        assert!(rep.settled.is_empty() && out.is_empty());
        assert_eq!(wl.len(), 1, "still watched");
    }

    /// The ambiguity SKIP is bypassed when the chain can price the leg — ambiguity is only
    /// unresolvable without chain data.
    #[test]
    fn chain_resolves_what_the_ambiguity_guard_can_only_skip() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let deps = StubDeps::new(vec![vec![
            pos_idx("0xA", "tokWin", 100.0, true, false, 0),
            pos_idx("0xA", "tokLose", 40.0, true, false, 1), // both flagged ⇒ ambiguous
        ]])
        .with_chain("0xA", vec![1, 0], 1);
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        assert!(rep.skipped_ambiguous.is_empty(), "the chain answers what the guard could not");
        assert_eq!(rep.settled.len(), 2);
        let px: BTreeMap<&str, u64> =
            fills(&out).iter().map(|f| (f.symbol.as_str(), f.last_px.to_bits())).collect();
        assert_eq!(px["tokWin"], 1.0f64.to_bits());
        assert_eq!(px["tokLose"], 0.0f64.to_bits());
    }

    /// A SPLIT resolution pays BOTH legs 0.5 — a payout the boolean `redeemable` flag cannot even
    /// express, so this case is only reachable through the chain.
    #[test]
    fn chain_settles_a_split_resolution_at_half() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let deps = StubDeps::new(vec![vec![pos_idx("0xS", "tokA", 10.0, false, false, 0)]])
            .with_chain("0xS", vec![1, 1], 2);
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        let fs = fills(&out);
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].last_px.to_bits(), 0.5f64.to_bits());
    }

    /// The at-most-once ledger still governs the chain path, and the chain path still respects the
    /// local-book override — the oracle changes the PAYOUT, never the bookkeeping.
    #[test]
    fn chain_path_keeps_the_ledger_and_local_override_contracts() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let local: BTreeMap<String, f64> = [("tokWin".to_string(), 3.0)].into_iter().collect();
        let deps = StubDeps::with_local(
            vec![vec![pos_idx("0xA", "tokWin", 100.0, false, false, 1)]],
            local,
        )
        .with_chain("0xA", vec![0, 1], 1);
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let r1 = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        assert_eq!(r1.settled, vec!["tokWin".to_string()]);
        assert_eq!(fills(&out)[0].last_qty, 3.0, "closes the LOCAL size");
        assert_eq!(fills(&out)[0].last_px.to_bits(), 1.0f64.to_bits());

        let mut out2 = Vec::new();
        let r2 = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out2));
        assert!(r2.settled.is_empty() && out2.is_empty(), "not settled twice");
    }

    /// `outcome_index` rides from the data-api row onto the watch entry — without it the chain
    /// verdict could not be applied to the right leg.
    #[test]
    fn watch_entry_carries_the_outcome_index() {
        let mut wl = ResolveWatchlist::new();
        wl.upsert_from_positions(&[pos_idx("0xA", "tokA", 1.0, false, false, 1)]);
        assert_eq!(wl.get("tokA").unwrap().outcome_index, Some(1));
        // refreshed on re-upsert, like every other field
        wl.upsert_from_positions(&[pos_idx("0xA", "tokA", 1.0, false, false, 0)]);
        assert_eq!(wl.get("tokA").unwrap().outcome_index, Some(0));
    }

    /// An UNREADABLE `outcomeIndex` rides across as `None` — the row is still watched (it is a held
    /// position; skipping it would let `prune_flat` evict it), the slot alone is unknown.
    #[test]
    fn watch_entry_carries_an_unreadable_outcome_index_as_none() {
        let mut wl = ResolveWatchlist::new();
        let mut p = pos_idx("0xA", "tokA", 1.0, false, false, 1);
        p.outcome_index = None;
        wl.upsert_from_positions(&[p]);
        assert_eq!(wl.get("tokA").unwrap().outcome_index, None);
        assert_eq!(wl.len(), 1, "still watched — only the slot is unknown");
    }

    /// **The `crate::resolve` half of the `outcomeIndex` hardening.** The chain has resolved this
    /// condition `[1, 0]` and we hold the LOSING leg, whose data-api row reports `redeemable: true`
    /// like every row of a resolved condition. With the slot unreadable, the chain verdict cannot
    /// be applied — and the `redeemable`-derived fallback would put this very token in `winners`
    /// and settle it at 1.0, writing a fabricated realized PnL. It must settle NOTHING and stay
    /// watched, so a later snapshot with a readable slot can price it correctly.
    ///
    /// Reported in [`SettleTickReport::skipped_unpriced`], NOT `skipped_ambiguous`: under
    /// [`PayoutSource::Chain`] an unreadable slot and an out-of-range slot are the same refusal —
    /// a resolved verdict that cannot be read against this leg — and `skipped_ambiguous` is the
    /// flag source's own, unrelated guard. `unreadable_and_out_of_range_slots_share_one_skip_field`
    /// pins that they are one field; `chain_source_skips_a_resolved_condition_it_cannot_price_this_leg_of`
    /// covers the out-of-range twin.
    #[test]
    fn a_chain_resolved_condition_with_an_unreadable_slot_settles_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        // The held LOSING leg: slot 1 of a `[1, 0]` resolution, flagged `redeemable: true` like
        // every row of a resolved condition — but the page did not give us a readable slot.
        let mut held = pos_idx("0xA", "tokLose", 10.0, true, false, 1);
        held.outcome_index = None;
        let deps = StubDeps::new(vec![vec![held]]).with_chain("0xA", vec![1, 0], 1);
        assert_eq!(deps.payout_source(), PayoutSource::Chain, "the production source");
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

        assert!(out.is_empty(), "no settlement fill — a guessed payout is a fabricated PnL");
        assert!(rep.settled.is_empty() && rep.chain_priced.is_empty());
        assert_eq!(rep.skipped_unpriced, vec!["tokLose".to_string()], "refused, and reported");
        assert!(rep.skipped_ambiguous.is_empty(), "the flag guard is not what refused here");
        assert!(!ledger.contains("0xA", "tokLose"), "not tombstoned — a later page can price it");
        assert_eq!(wl.len(), 1, "still watched");

        // The control: the SAME snapshot with the slot readable settles the loser at 0.0, proving
        // the refusal above was about the unreadable slot and nothing else.
        let deps2 = StubDeps::new(vec![vec![pos_idx("0xA", "tokLose", 10.0, true, false, 1)]])
            .with_chain("0xA", vec![1, 0], 1);
        let mut wl2 = ResolveWatchlist::new();
        let mut out2 = Vec::new();
        let rep2 = settle_once(&deps2, "0xproxy", &mut wl2, &ledger, &mut sink(&mut out2));
        assert_eq!(rep2.settled, vec!["tokLose".to_string()]);
        assert_eq!(fills(&out2)[0].last_px.to_bits(), 0.0f64.to_bits(), "priced 0, not 1");
    }

    /// **The rebase witness.** Two PRs independently added a refusal for "the chain resolved this
    /// condition but I cannot price this leg": an UNREADABLE slot (`outcome_index: None`) and an
    /// OUT-OF-RANGE slot (`Some(3)` against a 2-slot verdict). They are ONE concern — same
    /// authority, same predicate (`chain_payout.is_none()` with a verdict in hand), same
    /// refusal, same recovery — so they report through ONE field. This pins that: both inputs land
    /// in `skipped_unpriced`, neither in `skipped_ambiguous`, and both leave the leg watched and
    /// unmarked. The operator tells them apart from the WARN log's `outcome_index`, which is where
    /// the distinction is actually actionable (`None` ⇒ refetch the degraded page; `Some(i)` ⇒
    /// anomalous chain data).
    #[test]
    fn unreadable_and_out_of_range_slots_share_one_skip_field() {
        let mut unreadable = pos_idx("0xA", "tok", 10.0, true, false, 1);
        unreadable.outcome_index = None;
        let out_of_range = pos_idx("0xA", "tok", 10.0, true, false, 3);

        for (label, held) in [("unreadable", unreadable), ("out-of-range", out_of_range)] {
            let dir = tempfile::tempdir().unwrap();
            let ledger = SettlementLedger::open(dir.path().join("s.txt"));
            // The SAME `[1, 0]` verdict in both halves — only the leg's own slot differs.
            let deps = StubDeps::new(vec![vec![held]]).with_chain("0xA", vec![1, 0], 1);
            let mut wl = ResolveWatchlist::new();
            let mut out = Vec::new();
            let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

            assert!(out.is_empty(), "{label}: nothing emitted");
            assert_eq!(rep.skipped_unpriced, vec!["tok".to_string()], "{label}: one field");
            assert!(rep.skipped_ambiguous.is_empty(), "{label}: not the flag guard");
            assert!(rep.settled.is_empty(), "{label}: nothing settled");
            assert!(!ledger.contains("0xA", "tok"), "{label}: unmarked, so a later pass retries");
            assert_eq!(wl.len(), 1, "{label}: still watched");
        }
    }
}
