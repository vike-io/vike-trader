//! `positions` — Polymarket data-api position discovery for CTF auto-redeem
//! (docs/superpowers/specs/2026-07-11-ctf-redeem-design.md).
//!
//! Field names PINNED (Step 0 of task-2-brief.md) against THREE independent sources, all agreeing:
//! - Official docs: `docs.polymarket.com/api-reference/core/get-current-positions-for-a-user`
//! - Community mirror of the Data API docs (example `/positions` response), a widely cited gist:
//!   `gist.github.com/shaunlebron/0dd3338f7dea06b8e9f8724981bb13bf`
//! - A working TypeScript redeem script that fetches+filters `/positions` directly:
//!   `gist.github.com/tylerthebuildor/fe48617cc2a30c123ab175e1e65b57cf`
//!
//! Confirmed shape: `conditionId` / `asset` / `size` / `redeemable` / `outcomeIndex` / `title`
//! (plus `curPrice`, added below — see the winner section).
//! **Deviation from the brief's guess:** the neg-risk flag is `negativeRisk`, NOT `negRisk` — all
//! three sources agree on `negativeRisk`; the brief's `negRisk` guess was wrong and is corrected
//! here. (`OrderBookTrade/rs-builder-relayer-client`, the brief's other named reference, turned out
//! to cover the gasless relayer's redeem/split/merge calls, not the data-api `/positions` read — it
//! has no `Position`-shaped type to cross-check against.)
//!
//! # ⚠ `redeemable: true` is NOT a winner flag
//!
//! This is the governing fact of this module, and it was learned the expensive way — twice, from
//! two independent live reads of this repo's own mainnet account (funder
//! `0x107C01D04Fd68557ACd52E89dD01972b22803aD5`; 2026-07-23 and again 2026-08-02, unchanged):
//!
//! | conditionId | market | outIdx | `curPrice` | on-chain `payoutNumerators` | truth |
//! |---|---|---|---|---|---|
//! | `0x13bf6efb…` | Solana Up/Down Jun 9 | 1 | 0 | `[1, 0]` | LOSER |
//! | `0xbf336239…` | Doge Up/Down Jun 11 | 0 | 0 | `[0, 1]` | LOSER |
//! | `0x5a71ff88…` | Doge Up/Down Jun 9 | 0 | 0 | `[0, 1]` | LOSER |
//! | `0xf361b0aa…` | BNB Up/Down Jun 12 | 1 | 1 | `[0, 1]` | **WINNER, $5** |
//!
//! **All four report `redeemable: true`.** Every position of a RESOLVED condition does — losers
//! included. So `redeemable` means "this condition has resolved and this token CAN be handed to the
//! redeem contract (possibly for zero)"; it is a RESOLUTION flag. Reading it as a winner flag
//! settles the three losers at 1.0 and fabricates **≈$29.11** of profit that never existed, and —
//! the defect this module was fixed for — makes the auto-redeem poller submit FOUR relayer calls
//! for this wallet, three of which pay nothing and each of which would write a permanent ledger
//! tombstone for a non-event. That table is pinned verbatim in this module's `live_wallet_fixture`
//! tests so the loser-selection bug cannot come back.
//!
//! # Where winner truth comes from (a deliberate, stated choice)
//!
//! [`WinnerSource`] names the two candidates. They are NOT interchangeable, and the safe one is the
//! default **precisely because it is the one that needs the network**:
//!
//! 1. [`WinnerSource::Chain`] — **the default, and the only authoritative source.** The CTF's own
//!    `payoutNumerators(bytes32,uint256)` mapping (selector `0x0504c814`), read through
//!    [`crate::exec_plane::settlement::chain::ChainOracle::resolution`]. It is the contract state the redeem itself pays
//!    out of, so it cannot disagree with the money. It also expresses payouts no flag can: a SPLIT
//!    resolution (`[1,1]` over denominator 2) pays 0.5 to BOTH legs. Cost, per
//!    [`crate::exec_plane::settlement::chain::PolygonRpc::condition_resolution`]: an UNRESOLVED condition is exactly **one**
//!    `eth_call` — the `payoutDenominator` read returns 0 and short-circuits before
//!    `outcomeSlotCount`/`payoutNumerators` are asked for; a RESOLVED binary is
//!    `1 + 1 + 2 = four` (denominator, slot count, then one numerator per slot), paid **once**,
//!    because the oracle CACHES resolved verdicts (a resolved condition never un-resolves). So a
//!    settled watchlist costs zero calls per tick thereafter, and an unsettled one costs one call
//!    per open condition per pass.
//! 2. [`WinnerSource::CurPriceOnly`] — **an explicit opt-out, never the default.** The data-api's
//!    `curPrice` convenience field, already in the `/positions` payload, so it costs nothing. It
//!    agreed with the chain on 4/4 rows above — which is evidence, not proof. It is a *derived
//!    display* number computed by an off-chain indexer from a market's last state, not the payout
//!    the redeem contract reads: it can lag a fresh resolution, it is absent on a partial/degraded
//!    page (⇒ [`Payout::Unknown`], never a guessed 0), and no published contract binds it to
//!    `payoutNumerators`. **When it disagrees with the chain, the chain is right and `curPrice` is
//!    wrong** — there is no case where a data-api convenience field overrules contract state (a
//!    pinned test states this: `when_cur_price_disagrees_with_the_chain_the_chain_decides`).
//!    Choosing it means accepting one of two errors: a stale or absent price on a real winner
//!    delays a redeem by a tick (self-healing, see below), and a stale non-zero price on a loser
//!    costs one wasted relayer call. It exists for a host that genuinely cannot reach Polygon RPC,
//!    and a caller has to say its name to get it.
//!
//! # Every unknown fails CLOSED — including the one that selects the slot
//!
//! An RPC blip, a condition the chain says is not resolved yet, an **unreadable `outcomeIndex`**
//! (absent, non-numeric, or out of `u32` — see `parse_outcome_index`), an outcome index outside the
//! payout vector, and a missing `curPrice` all yield [`Payout::Unknown`], which [`redeemable`]
//! excludes.
//!
//! `outcomeIndex` earns its own mention because under the chain-first design above it is strictly
//! MORE load-bearing than `curPrice`: it chooses WHICH numerator is read. The pre-fix
//! `unwrap_or(0) as u32` answered every unreadable shape with slot `0` — and on the Solana row of
//! the live table above, resolved `[1, 0]` with slot **1** held, slot 0 is exactly the paying one.
//! So a degraded page did not merely lose information, it converted the held loser into a "winner",
//! submitted a relayer redeem worth nothing, and wrote a permanent at-most-once tombstone for it.
//! (The `as u32` cast made it worse: `outcomeIndex: 4294967296` WRAPPED to 0.) The field is now
//! `Option<u32>`, and an unreadable slot is `Unknown` under BOTH winner sources — see
//! [`payout_of`].
//!
//! Skipping is never a forfeit: discovery re-runs every tick and the position stays `redeemable`
//! until it is actually redeemed, so the only cost of a wrong `Unknown` is one tick of delay. The
//! opposite error — redeeming on a guess — burns a relayer call, and on the [`crate::exec_plane::settlement::resolve`] side
//! would fabricate realized PnL. This is the same refuse-to-guess rule
//! [`crate::exec_plane::settlement::resolve::ambiguous_conditions`] and [`crate::exec_plane::settlement::chain::join_settlement`] already follow.

use crate::config::DATA_API_BASE;
use crate::exec_plane::settlement::chain::ChainResolution;

/// One held position as the data-api reports it (fields confirmed in this module's doc).
#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    pub condition_id: String,
    pub asset: String,
    pub size: f64,
    /// ⚠ A RESOLUTION flag, not a winner flag — see this module's doc. Every position of a resolved
    /// condition reports `true`, losers included.
    pub redeemable: bool,
    pub neg_risk: bool,
    /// Which outcome SLOT of the condition this row is (data-api `outcomeIndex`) — the index the
    /// chain's `payoutNumerators` vector is keyed by, and the slot a neg-risk redeem's AMOUNTS
    /// array is addressed to. `None` when the payload's value is absent, non-numeric, or outside
    /// `u32`.
    ///
    /// **`Option`, not a `0` fallback — and not a skipped row either.** See
    /// `parse_outcome_index` for why an unreadable slot must stay unreadable, and
    /// [`parse_positions`] for why the row is kept anyway.
    pub outcome_index: Option<u32>,
    pub title: String,
    /// data-api `curPrice` — the indexer's current mark for this outcome token (0 or 1 once the
    /// condition has resolved). `None` when the field is absent from the payload, which must stay
    /// distinguishable from a genuine `Some(0.0)`: absent is [`Payout::Unknown`] (skip and retry),
    /// zero is [`Payout::Loser`] (never redeem). Only consulted under
    /// [`WinnerSource::CurPriceOnly`].
    pub cur_price: Option<f64>,
}

/// Read the data-api `outcomeIndex` field. `None` — never a guessed `0` — for **every** shape that
/// is not an exact, in-range, non-negative integer: absent, `null`, a float, a bool, an object, a
/// non-numeric string, a negative, or a value beyond `u32`.
///
/// # Why the number-OR-quoted-string tolerance is not invented leniency
///
/// It is this repo's already-pinned reading of **this very endpoint**: `recon_client`'s `num`/
/// `num_val` parse the SAME `GET /positions?user=<funder>` rows and deliberately accept both
/// encodings — *"a numeric field that may arrive as a JSON number OR a decimal string (Polymarket
/// quotes most sizes/prices)"* — as does `user_ws`, where every numeric arrives quoted. So `"1"` is
/// a shape this venue's own wire is documented in-tree to produce, and reading it as `1` is exact:
/// a string that parses to an integer *is* that integer, so the tolerance cannot invent a slot.
/// What it must never do is fold an UNPARSEABLE value to a number, which is the whole defect below.
///
/// # Why `None` and not `0`
///
/// `unwrap_or(0)` is a silent claim to hold slot 0. On a condition the chain resolved `[1, 0]`,
/// `payout_for_index(0)` is `1.0` ⇒ [`Payout::Winner`] ⇒ the poller hands a **worthless** position
/// to the relayer and `RedeemLedger` writes a permanent tombstone for it, and `neg_risk_amounts`
/// addresses the redeem to the wrong slot. That is the same failure `cur_price` is an
/// [`Option`] to avoid, one field down — and under the chain-first design `outcomeIndex` is
/// strictly MORE load-bearing than `curPrice`, because it selects which numerator is read.
///
/// The `u32` bound is load-bearing too, not tidiness: the pre-fix `as u32` cast **wrapped**, so
/// `outcomeIndex: 4294967296` (2³²) truncated to exactly `0` and reached the relayer as a
/// "slot 0 winner". `u32::try_from` turns that into `None`.
fn parse_outcome_index(v: Option<&serde_json::Value>) -> Option<u32> {
    let v = v?;
    // Number first, quoted decimal second — the `recon_client::num_val` shape, narrowed from f64 to
    // an exact integer so `"1.5"`/`1.5` cannot round into a slot.
    let n = v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))?;
    u32::try_from(n).ok()
}

/// Parse the `/positions` JSON array. A malformed entry (missing `conditionId`/`asset`) is skipped
/// (tolerant, like the venue mappers) rather than failing the whole batch.
///
/// **An unreadable `outcomeIndex` does NOT skip the row** — it lands as `outcome_index: None` (see
/// `parse_outcome_index`). The two are treated differently on purpose: a row without a
/// `conditionId`/`asset` names nothing this crate can hold, but a row with an unreadable slot is
/// still a *held position*, and [`crate::exec_plane::settlement::resolve::ResolveWatchlist::prune_flat`] evicts every
/// watched token the snapshot no longer reports with `size > 0`. Skipping the row would therefore
/// make a degraded page look like a wallet that had SOLD the position and silently drop it from
/// settlement tracking — the exact "absent must stay distinguishable" failure this module refuses
/// elsewhere. Keeping it costs nothing: [`Payout::Unknown`] already excludes it from every redeem.
pub fn parse_positions(json: &serde_json::Value) -> Vec<Position> {
    let arr = match json.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    for e in arr {
        let cond = e.get("conditionId").and_then(|v| v.as_str());
        let asset = e.get("asset").and_then(|v| v.as_str());
        let (Some(condition_id), Some(asset)) = (cond, asset) else { continue };
        out.push(Position {
            condition_id: condition_id.to_string(),
            asset: asset.to_string(),
            size: e.get("size").and_then(|v| v.as_f64()).unwrap_or(0.0),
            redeemable: e.get("redeemable").and_then(|v| v.as_bool()).unwrap_or(false),
            neg_risk: e.get("negativeRisk").and_then(|v| v.as_bool()).unwrap_or(false),
            // ABSENT/garbage stays absent — see `parse_outcome_index`. `unwrap_or(0)` here claimed
            // slot 0, which on a `[1, 0]` resolution reads Winner and redeems a loser.
            outcome_index: parse_outcome_index(e.get("outcomeIndex")),
            title: e.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            // ABSENT stays absent — `unwrap_or(0.0)` here would turn a degraded page into a wallet
            // full of "losers", which is exactly the kind of silent guess this module refuses.
            cur_price: e.get("curPrice").and_then(|v| v.as_f64()),
        });
    }
    out
}

/// What one position is actually worth at redemption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Payout {
    /// Pays something (`> 0` collateral per token) — redeeming it moves real money.
    Winner,
    /// Resolved and worth zero — redeeming it burns a relayer call and a ledger row for $0.
    Loser,
    /// The source could not answer: an RPC failure, a condition the chain says is not resolved yet,
    /// an **unreadable** outcome index (`outcome_index: None`), an outcome index outside the payout
    /// vector, or an absent `curPrice`. Treated as "not now" — [`redeemable`] excludes it and the
    /// next tick asks again.
    Unknown,
}

/// Where [`payout_of`] reads winner truth from. **The default is [`Chain`](WinnerSource::Chain);
/// [`CurPriceOnly`](WinnerSource::CurPriceOnly) is an explicit opt-out.** See this module's doc for
/// the full argument.
pub enum WinnerSource<'a> {
    /// AUTHORITATIVE: the CTF's own payout mappings, via a `conditionId -> resolution` lookup
    /// (normally [`crate::exec_plane::settlement::chain::ChainOracle::resolution`], which caches and returns `None` on a
    /// transport failure rather than asserting "unresolved").
    Chain(&'a dyn Fn(&str) -> Option<ChainResolution>),
    /// OPT-OUT: the data-api's `curPrice`. Network-free, and only as good as the indexer.
    CurPriceOnly,
}

/// One position's payout verdict under `source`. Pure: the `Chain` variant does its I/O inside the
/// caller-supplied closure, so the whole decision table is testable offline.
///
/// **An unreadable `outcome_index` is [`Payout::Unknown`] under BOTH sources**, including
/// [`WinnerSource::CurPriceOnly`], which does not itself need the slot to tell a winner from a
/// loser. That is deliberate: [`Payout::Winner`] is not an opinion, it is a licence to submit a
/// relayer redeem — and a neg-risk redeem's AMOUNTS array is addressed BY that slot
/// (`auto_redeem::neg_risk_amounts`). Knowing a position pays is useless if the redeem cannot be
/// pointed at the right leg, so the gate is uniform rather than per-source.
pub fn payout_of(p: &Position, source: &WinnerSource<'_>) -> Payout {
    // No readable slot ⇒ no verdict, under any source. Fails closed before the source is consulted.
    let Some(idx) = p.outcome_index else { return Payout::Unknown };
    match source {
        WinnerSource::Chain(lookup) => match lookup(&p.condition_id) {
            // `payout_for_index` is `None` for an unresolved condition or an out-of-range slot;
            // both are genuinely unknown, never "worthless".
            Some(r) => match r.payout_for_index(idx) {
                Some(x) if x > 0.0 => Payout::Winner,
                Some(_) => Payout::Loser,
                None => Payout::Unknown,
            },
            None => Payout::Unknown,
        },
        // A resolved market marks 0 or 1, so `> 0.0` is the whole test. A dust non-zero mark (never
        // observed) would read Winner and cost one wasted relayer call — the cheap direction.
        WinnerSource::CurPriceOnly => match p.cur_price {
            Some(x) if x > 0.0 => Payout::Winner,
            Some(_) => Payout::Loser,
            None => Payout::Unknown,
        },
    }
}

/// STEP 1, network-free: the positions whose condition the data-api says has RESOLVED and that we
/// still hold and have not already settled. **These are candidates, not winners** — see this
/// module's doc; on the live fixture above this returns all four rows, three of them worthless.
///
/// Exposed on its own for exactly one reason: a caller that wants to say "the wallet had resolved
/// positions but none of them pays" needs both sets. `tests/redeem_smoke.rs` uses it to FAIL LOUDLY
/// instead of quietly proving less than it claims.
pub fn resolved_candidates(
    positions: &[Position],
    already_redeemed: impl Fn(&str) -> bool,
) -> Vec<Position> {
    positions
        .iter()
        .filter(|p| p.redeemable && p.size > 0.0 && !already_redeemed(&p.condition_id))
        .cloned()
        .collect()
}

/// STEP 2 — **the filter the redeem path must use**: of [`resolved_candidates`], the ones that
/// actually PAY, per `source`. Includes BOTH binary AND neg-risk winners — the poller routes each by
/// its `Position.neg_risk` flag to the right contract (CTF vs NegRiskAdapter). `already_redeemed` is
/// a predicate (not a concrete `RedeemLedger`) so discovery stays decoupled from the ledger type.
///
/// [`Payout::Unknown`] is EXCLUDED (fail closed). A dropped winner is retried on the next tick; a
/// submitted loser is a wasted relayer call and a permanent tombstone for a non-event.
pub fn redeemable(
    positions: &[Position],
    already_redeemed: impl Fn(&str) -> bool,
    source: &WinnerSource<'_>,
) -> Vec<Position> {
    redeemable_by(positions, already_redeemed, |p| payout_of(p, source))
}

/// The same filter, over an arbitrary per-position payout verdict. This is the ONE implementation;
/// [`redeemable`] is the [`WinnerSource`]-policy entry point onto it.
///
/// It exists because [`crate::exec_plane::settlement::auto_redeem::RedeemDeps`] owns the poller's whole outside world behind
/// one trait — including which winner source is configured — so the poller injects
/// `deps.payout(p)` here rather than re-deriving a `WinnerSource` it does not hold. That also makes
/// the poller's own tests able to script per-position verdicts with no chain and no data-api.
pub fn redeemable_by(
    positions: &[Position],
    already_redeemed: impl Fn(&str) -> bool,
    payout: impl Fn(&Position) -> Payout,
) -> Vec<Position> {
    resolved_candidates(positions, already_redeemed)
        .into_iter()
        .filter(|p| match payout(p) {
            Payout::Winner => true,
            Payout::Loser => {
                tracing::debug!(
                    target: "vike_polymarket::positions",
                    condition_id = %p.condition_id, outcome_index = ?p.outcome_index, title = %p.title,
                    "redeemable=true but the payout source says this leg is worthless — not redeeming"
                );
                false
            }
            Payout::Unknown => {
                tracing::warn!(
                    target: "vike_polymarket::positions",
                    condition_id = %p.condition_id, outcome_index = ?p.outcome_index, title = %p.title,
                    "payout source could not answer for a resolved position — skipped this pass, will retry"
                );
                false
            }
        })
        .collect()
}

/// Live discovery: GET `DATA_API_BASE/positions?user=<proxy>` through the proxy-aware agent —
/// reachable via the Dublin tunnel like the other Polymarket reads (`egress::agent()` routes
/// through the SOCKS proxy when enabled).
pub struct PositionsClient;

impl PositionsClient {
    pub fn list(proxy_addr: &str) -> Result<Vec<Position>, String> {
        let json =
            crate::egress::get_json(DATA_API_BASE, "/positions", &format!("user={proxy_addr}"))?;
        Ok(parse_positions(&json))
    }
}

#[path = "positions_tests.rs"]
#[cfg(test)]
mod positions_tests;
