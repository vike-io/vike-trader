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
//!   ([`ChainResolveDeps`] over a [`crate::exec_plane::settlement::chain::ChainOracle`]). A leg's payout is the CTF's own
//!   `payoutNumerators` for its `outcome_index`, read through
//!   [`crate::exec_plane::settlement::chain::PolygonRpc::condition_resolution`]; `redeemable` is not consulted at all.
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
//! Downstream this also lands correctly in the journal: `vike_journal::materialize`'s order fold ignores
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

use crate::exec_plane::settlement::chain::{ChainOracle, ChainResolution};
use crate::exec_plane::settlement::positions::{Position, PositionsClient};

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
/// `crate::exec_plane::recon_client::settlement_fill_report` stamp the SAME string for the same settled
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
    /// setting. Deliberately NOT gated on [`crate::exec_plane::settlement::chain::chain_watch_enabled`]: that flag gates the
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
    ) -> Option<StopHandle> {
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
    ) -> Option<StopHandle> {
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
    ) -> Option<StopHandle> {
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

#[path = "resolve_tests.rs"]
#[cfg(test)]
mod resolve_tests;
