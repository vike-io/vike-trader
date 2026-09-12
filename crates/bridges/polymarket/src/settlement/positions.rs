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
//!    [`crate::chain::ChainOracle::resolution`]. It is the contract state the redeem itself pays
//!    out of, so it cannot disagree with the money. It also expresses payouts no flag can: a SPLIT
//!    resolution (`[1,1]` over denominator 2) pays 0.5 to BOTH legs. Cost, per
//!    [`crate::chain::PolygonRpc::condition_resolution`]: an UNRESOLVED condition is exactly **one**
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
//! opposite error — redeeming on a guess — burns a relayer call, and on the [`crate::resolve`] side
//! would fabricate realized PnL. This is the same refuse-to-guess rule
//! [`crate::resolve::ambiguous_conditions`] and [`crate::chain::join_settlement`] already follow.

use crate::chain::ChainResolution;

/// data-api base (positions/activity reads). Reachable via the Dublin tunnel like the other
/// Polymarket reads (`egress::agent()` routes through the SOCKS proxy when enabled).
pub const DATA_API: &str = crate::config::DATA_API_BASE;

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
/// still a *held position*, and [`crate::resolve::ResolveWatchlist::prune_flat`] evicts every
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
    /// (normally [`crate::chain::ChainOracle::resolution`], which caches and returns `None` on a
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
/// It exists because [`crate::auto_redeem::RedeemDeps`] owns the poller's whole outside world behind
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

/// Live discovery: GET `DATA_API/positions?user=<proxy>` through the proxy-aware agent.
pub struct PositionsClient;

impl PositionsClient {
    pub fn list(proxy_addr: &str) -> Result<Vec<Position>, String> {
        let json = crate::egress::get_json(DATA_API, "/positions", &format!("user={proxy_addr}"))?;
        Ok(parse_positions(&json))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn canned() -> serde_json::Value {
        serde_json::json!([
            {"conditionId":"0x01","asset":"111","size":100.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":0,"title":"BTC>100k","curPrice":1.0},
            {"conditionId":"0x02","asset":"222","size":50.0,"redeemable":false,"negativeRisk":false,"outcomeIndex":1,"title":"still trading","curPrice":0.4},
            {"conditionId":"0x03","asset":"333","size":10.0,"redeemable":true,"negativeRisk":true,"outcomeIndex":0,"title":"neg-risk win","curPrice":1.0},
            {"conditionId":"0x04","asset":"444","size":0.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":0,"title":"zero size","curPrice":1.0}
        ])
    }

    /// A `WinnerSource::Chain` backing table — the offline stand-in for `ChainOracle`. A conditionId
    /// absent from the table yields `None`, i.e. the RPC-blip shape.
    fn chain_table(rows: &[(&str, u128, &[u128])]) -> HashMap<String, ChainResolution> {
        rows.iter()
            .map(|(cid, den, nums)| {
                (
                    (*cid).to_string(),
                    ChainResolution {
                        condition_id: (*cid).to_string(),
                        denominator: *den,
                        numerators: nums.to_vec(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn parse_reads_all_positions() {
        let ps = parse_positions(&canned());
        assert_eq!(ps.len(), 4);
        assert_eq!(ps[0].condition_id, "0x01");
        assert!(ps[0].redeemable && ps[0].size == 100.0 && !ps[0].neg_risk);
        assert!(ps[2].neg_risk);
        assert_eq!(ps[0].cur_price, Some(1.0));
        assert_eq!(ps[0].outcome_index, Some(0));
        assert_eq!(ps[1].outcome_index, Some(1));
    }

    #[test]
    fn redeemable_includes_binary_and_neg_risk_winners() {
        let ps = parse_positions(&canned());
        let r = redeemable(&ps, |_| false, &WinnerSource::CurPriceOnly);
        // Includes BOTH the binary winner (0x01) and the neg-risk winner (0x03); still excludes
        // the unresolved row (0x02, redeemable=false) and the zero-size one (0x04).
        let cids: Vec<_> = r.iter().map(|p| p.condition_id.as_str()).collect();
        assert!(cids.contains(&"0x01") && cids.contains(&"0x03"), "both winners included");
        assert!(!cids.contains(&"0x02") && !cids.contains(&"0x04"));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn redeemable_excludes_already_redeemed() {
        let ps = parse_positions(&canned());
        let r = redeemable(&ps, |cid| cid == "0x01", &WinnerSource::CurPriceOnly);
        assert!(r.iter().all(|p| p.condition_id != "0x01"));
    }

    #[test]
    fn parse_skips_malformed_entries() {
        let json = serde_json::json!([
            {"asset":"111","size":100.0,"redeemable":true}, // missing conditionId
            {"conditionId":"0x05"}, // missing asset
            {"conditionId":"0x06","asset":"666","size":5.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":0,"title":"ok"},
            "not an object",
        ]);
        let ps = parse_positions(&json);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].condition_id, "0x06");
        assert_eq!(ps[0].cur_price, None, "an absent curPrice is None, not a guessed 0.0");
    }

    #[test]
    fn parse_non_array_returns_empty() {
        let ps = parse_positions(&serde_json::json!({"error": "bad user"}));
        assert!(ps.is_empty());
    }

    // =============================================================================================
    // `outcomeIndex` — THE LAST ROUTE BY WHICH A LOSER COULD REACH THE RELAYER.
    //
    // Pre-fix: `.and_then(|v| v.as_u64()).unwrap_or(0) as u32`. Every shape below yielded `0` — a
    // silent claim to hold slot 0 — and on a condition the chain resolved `[1, 0]` that reads
    // `payout_for_index(0) == 1.0` ⇒ Winner ⇒ a relayer redeem for a WORTHLESS position plus a
    // permanent at-most-once ledger tombstone, and a `neg_risk_amounts` array pointed at the wrong
    // slot. Every test here holds slot **1** of exactly that resolution: the truth is LOSER, and no
    // unreadable shape may ever say Winner.
    // =============================================================================================
    mod outcome_index_hardening {
        use super::*;

        /// The condition every test below is held against: resolved `[1, 0]` over denominator 1, so
        /// slot 0 pays 1.0 and slot 1 — the one actually held — pays 0.
        const COND: &str = "0xdead";

        /// One `/positions` row for `COND` with `outcomeIndex` set to `idx` VERBATIM (whatever JSON
        /// shape the caller passes), `curPrice: 1.0` (a stale/degraded indexer mark, so the
        /// `CurPriceOnly` arm has something to wrongly believe) and `negativeRisk: true` (so the
        /// mis-slotting of `neg_risk_amounts` is in scope too).
        fn row(idx: serde_json::Value) -> Vec<Position> {
            parse_positions(&serde_json::json!([{
                "conditionId": COND, "asset": "tok", "size": 10.0, "redeemable": true,
                "negativeRisk": true, "outcomeIndex": idx, "title": "held on slot 1", "curPrice": 1.0
            }]))
        }

        /// The same row with `outcomeIndex` OMITTED entirely — the partial/degraded page.
        fn row_without_the_field() -> Vec<Position> {
            parse_positions(&serde_json::json!([{
                "conditionId": COND, "asset": "tok", "size": 10.0, "redeemable": true,
                "negativeRisk": true, "title": "held on slot 1", "curPrice": 1.0
            }]))
        }

        fn resolved_one_zero() -> HashMap<String, ChainResolution> {
            chain_table(&[(COND, 1, &[1, 0])])
        }

        /// **The premise, stated as an assertion**: on this resolution slot 0 pays and slot 1 does
        /// not. Every `unwrap_or(0)` below would therefore have converted the held LOSER into a
        /// winner — this is the exact arithmetic that made the defect expensive.
        #[test]
        fn slot_zero_pays_and_slot_one_does_not_on_this_resolution() {
            let table = resolved_one_zero();
            let r = table.get(COND).unwrap();
            assert_eq!(r.payout_for_index(0), Some(1.0), "the slot a `0` fallback would claim");
            assert_eq!(r.payout_for_index(1), Some(0.0), "the slot actually held — worthless");
        }

        /// An absent field is `None`, never a guessed `0` — the `cur_price` discipline, applied to
        /// the strictly more load-bearing field.
        #[test]
        fn an_absent_outcome_index_is_none_not_slot_zero() {
            let ps = row_without_the_field();
            assert_eq!(ps.len(), 1, "the ROW is kept — only the slot is unknown");
            assert_eq!(ps[0].outcome_index, None);
        }

        /// A quoted decimal is read EXACTLY, per this venue's documented number-or-string wire
        /// (`recon_client::num_val` on these very rows). Reading `"1"` as `1` is the difference
        /// between correctly calling the held leg a loser and stalling on it forever.
        #[test]
        fn a_quoted_numeric_outcome_index_is_read_exactly() {
            assert_eq!(row(serde_json::json!("1"))[0].outcome_index, Some(1));
            assert_eq!(row(serde_json::json!("0"))[0].outcome_index, Some(0));
            assert_eq!(
                row(serde_json::json!(" 2 "))[0].outcome_index,
                Some(2),
                "whitespace trimmed"
            );
            assert_eq!(
                row(serde_json::json!(1))[0].outcome_index,
                Some(1),
                "plain number still works"
            );
        }

        /// Everything that is not an exact non-negative integer is `None`. Note `1.5` and `-1`:
        /// tolerating the STRING encoding must not become tolerating a rounded or signed one.
        #[test]
        fn a_non_integer_outcome_index_is_none() {
            for bad in [
                serde_json::json!("abc"),
                serde_json::json!(""),
                serde_json::json!("1abc"),
                serde_json::json!("1.5"),
                serde_json::json!(1.5),
                serde_json::json!(-1),
                serde_json::json!("-1"),
                serde_json::json!(null),
                serde_json::json!(true),
                serde_json::json!({}),
                serde_json::json!([1]),
            ] {
                assert_eq!(
                    row(bad.clone())[0].outcome_index,
                    None,
                    "garbage `{bad}` became a slot"
                );
            }
        }

        /// **The truncation bug, pinned.** `as u32` WRAPPED: `4294967296` (2³²) truncated to exactly
        /// `0` — the paying slot — so a huge value was not merely out of range, it was a forged
        /// claim to hold the winner. `u32::try_from` refuses it.
        #[test]
        fn an_out_of_u32_range_outcome_index_is_none_not_a_wrapped_zero() {
            // Multiples of 2³² wrapped to EXACTLY slot 0 — the paying slot. The premise is asserted
            // so this test states the harm, not just the fix.
            for huge in [4_294_967_296u64, 8_589_934_592] {
                assert_eq!(huge as u32, 0, "premise: the pre-fix cast wrapped this to slot 0");
                assert_eq!(row(serde_json::json!(huge))[0].outcome_index, None);
                assert_eq!(row(serde_json::json!(huge.to_string()))[0].outcome_index, None);
            }
            // Other out-of-range values wrapped to some arbitrary in-range slot instead (`u64::MAX`
            // → 4294967295). Harmless on today's 2-slot binaries, but still a fabricated index.
            for huge in [u64::MAX, u64::from(u32::MAX) + 1, 4_294_967_297] {
                assert_eq!(row(serde_json::json!(huge))[0].outcome_index, None);
                assert_eq!(row(serde_json::json!(huge.to_string()))[0].outcome_index, None);
            }
        }

        /// **THE REAL-MONEY PIN.** For every unreadable shape, on the `[1, 0]` resolution whose slot
        /// 1 is genuinely held: the verdict is `Unknown` under BOTH winner sources, and `redeemable`
        /// selects NOTHING. Not `Winner`, and — equally important — not `Loser` either: a
        /// fail-closed `Unknown` retries next tick, whereas asserting `Loser` on an unreadable slot
        /// would be the same guess in the other direction.
        #[test]
        fn no_unreadable_outcome_index_ever_produces_a_winner() {
            let table = resolved_one_zero();
            let lookup = |cid: &str| table.get(cid).cloned();
            let unreadable = [
                serde_json::json!(null),
                serde_json::json!("abc"),
                serde_json::json!(1.5),
                serde_json::json!(-1),
                serde_json::json!(true),
                serde_json::json!(4_294_967_296u64),
                serde_json::json!(u64::MAX),
            ];
            for bad in unreadable {
                for ps in [row(bad.clone()), row_without_the_field()] {
                    for src in [WinnerSource::Chain(&lookup), WinnerSource::CurPriceOnly] {
                        assert_eq!(
                            payout_of(&ps[0], &src),
                            Payout::Unknown,
                            "an unreadable outcomeIndex (`{bad}`) must fail closed, not guess"
                        );
                        assert!(
                            redeemable(&ps, |_| false, &src).is_empty(),
                            "an unreadable outcomeIndex (`{bad}`) reached the relayer"
                        );
                    }
                }
            }
        }

        /// The control: the SAME row with a readable slot behaves exactly as before — slot 1 is a
        /// `Loser` on the chain, and slot 0 (the one the old fallback invented) is a `Winner`. So
        /// the hardening removed the guess without dulling the verdict.
        #[test]
        fn a_readable_outcome_index_still_decides_normally() {
            let table = resolved_one_zero();
            let lookup = |cid: &str| table.get(cid).cloned();
            let held_loser = row(serde_json::json!(1));
            assert_eq!(payout_of(&held_loser[0], &WinnerSource::Chain(&lookup)), Payout::Loser);
            assert!(redeemable(&held_loser, |_| false, &WinnerSource::Chain(&lookup)).is_empty());

            let held_winner = row(serde_json::json!(0));
            assert_eq!(payout_of(&held_winner[0], &WinnerSource::Chain(&lookup)), Payout::Winner);
            assert_eq!(redeemable(&held_winner, |_| false, &WinnerSource::Chain(&lookup)).len(), 1);
        }

        /// An unreadable slot must NOT evict the position from settlement tracking — the reason
        /// `parse_positions` keeps the row instead of skipping it like a missing `conditionId`.
        /// `resolved_candidates` still sees it (so a caller can say "resolved but unclassifiable"),
        /// and `crate::resolve`'s `prune_flat` still sees a held `size > 0` row.
        #[test]
        fn an_unreadable_slot_keeps_the_row_visible_as_a_held_position() {
            let ps = row_without_the_field();
            assert_eq!(resolved_candidates(&ps, |_| false).len(), 1, "still a resolved candidate");
            assert!(ps[0].size > 0.0, "still reads as held, so prune_flat cannot evict it");
        }
    }

    // =============================================================================================
    // THE LIVE WALLET FIXTURE — the reproducible four-position table this module's doc pins.
    //
    // Funder 0x107C01D04Fd68557ACd52E89dD01972b22803aD5, read 2026-07-23 and again 2026-08-02.
    // ALL FOUR rows report `redeemable: true`; exactly ONE of them pays. Everything below exists so
    // the loser-selection bug cannot come back.
    // =============================================================================================
    mod live_wallet_fixture {
        use super::*;

        // The four real conditionIds, right-padded to a full 32 bytes (only the leading bytes were
        // recorded in the investigation; the padding is inert — nothing here parses the value).
        const SOL: &str = "0x13bf6efb00000000000000000000000000000000000000000000000000000000";
        const DOGE_JUN11: &str =
            "0xbf33623900000000000000000000000000000000000000000000000000000000";
        const DOGE_JUN9: &str =
            "0x5a71ff8800000000000000000000000000000000000000000000000000000000";
        const BNB: &str = "0xf361b0aa00000000000000000000000000000000000000000000000000000000";

        /// The `/positions` payload as the data-api returns it, IN DATA-API ORDER. That order is
        /// load-bearing for the D2 regression guard below: the single winner is LAST.
        fn live_positions_json() -> serde_json::Value {
            serde_json::json!([
                {"conditionId":SOL,"asset":"1001","size":29.11,"redeemable":true,"negativeRisk":false,"outcomeIndex":1,"title":"Solana Up or Down - June 9","curPrice":0.0},
                {"conditionId":DOGE_JUN11,"asset":"1002","size":10.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":0,"title":"Doge Up or Down - June 11","curPrice":0.0},
                {"conditionId":DOGE_JUN9,"asset":"1003","size":10.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":0,"title":"Doge Up or Down - June 9","curPrice":0.0},
                {"conditionId":BNB,"asset":"1004","size":5.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":1,"title":"BNB Up or Down - June 12","curPrice":1.0}
            ])
        }

        /// The chain's answer for the same four conditions: `payoutNumerators` over denominator 1.
        fn live_chain() -> HashMap<String, ChainResolution> {
            chain_table(&[
                (SOL, 1, &[1, 0]),        // held index 1 -> 0  LOSER
                (DOGE_JUN11, 1, &[0, 1]), // held index 0 -> 0  LOSER
                (DOGE_JUN9, 1, &[0, 1]),  // held index 0 -> 0  LOSER
                (BNB, 1, &[0, 1]),        // held index 1 -> 1  WINNER
            ])
        }

        fn chain_source(
            t: &HashMap<String, ChainResolution>,
        ) -> impl Fn(&str) -> Option<ChainResolution> + '_ {
            move |cid: &str| t.get(cid).cloned()
        }

        /// **The defect, pinned.** The `redeemable` flag alone selects ALL FOUR — this is exactly
        /// what the old filter returned, and what would have produced four relayer calls, three
        /// paying nothing.
        #[test]
        fn the_redeemable_flag_alone_selects_all_four_including_three_losers() {
            let ps = parse_positions(&live_positions_json());
            assert_eq!(ps.len(), 4);
            assert!(ps.iter().all(|p| p.redeemable), "every row reports redeemable: true");
            assert_eq!(
                resolved_candidates(&ps, |_| false).len(),
                4,
                "the flag is a RESOLUTION flag: it cannot tell the winner from the losers"
            );
        }

        /// The fix: the on-chain source keeps exactly the BNB winner.
        #[test]
        fn the_chain_source_selects_only_the_one_real_winner() {
            let ps = parse_positions(&live_positions_json());
            let table = live_chain();
            let lookup = chain_source(&table);
            let r = redeemable(&ps, |_| false, &WinnerSource::Chain(&lookup));
            assert_eq!(r.len(), 1, "one winner, not four");
            assert_eq!(r[0].condition_id, BNB);
            assert_eq!(r[0].size, 5.0, "the $5 BNB position");
        }

        /// **The D2 regression guard.** In data-api order the winner is LAST, so
        /// `candidates.iter().find(|p| !p.neg_risk)` over the FLAG-only set picks the Solana LOSER.
        /// Over the winner-filtered set the same `find` can only ever pick a winner.
        #[test]
        fn first_binary_of_the_flag_set_is_a_loser_but_of_the_winner_set_is_the_winner() {
            let ps = parse_positions(&live_positions_json());
            let flag_only = resolved_candidates(&ps, |_| false);
            assert_eq!(
                flag_only.iter().find(|p| !p.neg_risk).map(|p| p.condition_id.as_str()),
                Some(SOL),
                "the pre-fix smoke selected the Solana loser and redeemed $0"
            );

            let table = live_chain();
            let lookup = chain_source(&table);
            let winners = redeemable(&ps, |_| false, &WinnerSource::Chain(&lookup));
            assert_eq!(
                winners.iter().find(|p| !p.neg_risk).map(|p| p.condition_id.as_str()),
                Some(BNB),
                "the winner-filtered set has no loser to pick"
            );
        }

        /// `curPrice` agreed with the chain on 4/4 of this sample — the measurement that makes the
        /// opt-out defensible, pinned so a future divergence in the fixture is visible.
        #[test]
        fn cur_price_agrees_with_the_chain_on_all_four_rows() {
            let ps = parse_positions(&live_positions_json());
            let table = live_chain();
            let lookup = chain_source(&table);
            for p in &ps {
                assert_eq!(
                    payout_of(p, &WinnerSource::Chain(&lookup)),
                    payout_of(p, &WinnerSource::CurPriceOnly),
                    "sources disagree on {} ({})",
                    p.title,
                    p.condition_id
                );
            }
            let chain_set = redeemable(&ps, |_| false, &WinnerSource::Chain(&lookup));
            let price_set = redeemable(&ps, |_| false, &WinnerSource::CurPriceOnly);
            assert_eq!(chain_set, price_set);
        }

        /// Agreement is NOT a licence to trust `curPrice`: when the two disagree the chain wins,
        /// because the chain is what the redeem contract pays out of. A stale indexer marking the
        /// Solana loser at 1.0 must not make it redeemable.
        #[test]
        fn when_cur_price_disagrees_with_the_chain_the_chain_decides() {
            let mut ps = parse_positions(&live_positions_json());
            ps[0].cur_price = Some(1.0); // stale indexer: the Solana LOSER now marks 1.0
            let table = live_chain();
            let lookup = chain_source(&table);

            let chain_set = redeemable(&ps, |_| false, &WinnerSource::Chain(&lookup));
            assert_eq!(chain_set.len(), 1);
            assert_eq!(chain_set[0].condition_id, BNB, "the chain still says only BNB pays");

            let price_set = redeemable(&ps, |_| false, &WinnerSource::CurPriceOnly);
            assert_eq!(price_set.len(), 2, "the opt-out believes the stale mark — its stated cost");
        }

        /// An RPC blip is `Unknown`, and `Unknown` redeems NOTHING. The next tick asks again, so
        /// this is a delay, never a forfeit.
        #[test]
        fn an_unreachable_chain_redeems_nothing() {
            let ps = parse_positions(&live_positions_json());
            let down = |_: &str| None; // every lookup fails
            let r = redeemable(&ps, |_| false, &WinnerSource::Chain(&down));
            assert!(r.is_empty(), "fail closed: no winner source, no relayer call");
            for p in &ps {
                assert_eq!(payout_of(p, &WinnerSource::Chain(&down)), Payout::Unknown);
            }
        }

        /// A condition the chain says has NOT resolved (denominator 0) while the data-api flags it
        /// redeemable is a contradiction, not a winner — `Unknown`, skip.
        #[test]
        fn a_chain_unresolved_condition_is_unknown_not_a_winner() {
            let ps = parse_positions(&live_positions_json());
            let table = chain_table(&[(BNB, 0, &[])]);
            let lookup = chain_source(&table);
            assert_eq!(
                payout_of(&ps[3], &WinnerSource::Chain(&lookup)),
                Payout::Unknown,
                "denominator 0 means the CTF has not been told the outcome yet"
            );
            assert!(redeemable(&ps, |_| false, &WinnerSource::Chain(&lookup)).is_empty());
        }

        /// An outcome index that is READABLE but outside the payout vector cannot be priced —
        /// `Unknown`, not a guessed 0. Distinct from an UNREADABLE index (`None`, see
        /// `outcome_index_hardening`): this one fails at `payout_for_index`, that one before the
        /// source is consulted at all.
        #[test]
        fn an_out_of_range_outcome_index_is_unknown() {
            let mut ps = parse_positions(&live_positions_json());
            ps[3].outcome_index = Some(7);
            let table = live_chain();
            let lookup = chain_source(&table);
            assert_eq!(payout_of(&ps[3], &WinnerSource::Chain(&lookup)), Payout::Unknown);
        }

        /// An absent `curPrice` (a degraded/partial page) is `Unknown`, NOT a loser — the whole
        /// reason the field is `Option<f64>`.
        #[test]
        fn an_absent_cur_price_is_unknown_not_a_loser() {
            let mut ps = parse_positions(&live_positions_json());
            ps[3].cur_price = None;
            assert_eq!(payout_of(&ps[3], &WinnerSource::CurPriceOnly), Payout::Unknown);
            assert!(redeemable(&ps, |_| false, &WinnerSource::CurPriceOnly).is_empty());
        }

        /// A SPLIT resolution (`[1,1]` over 2) pays 0.5 to BOTH legs — a payout no boolean flag can
        /// express, and both legs are genuine winners.
        #[test]
        fn a_split_resolution_makes_both_legs_winners() {
            let ps = parse_positions(&serde_json::json!([
                {"conditionId":"0xsplit","asset":"1","size":10.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":0,"title":"split","curPrice":0.5},
                {"conditionId":"0xsplit","asset":"2","size":10.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":1,"title":"split","curPrice":0.5}
            ]));
            let table = chain_table(&[("0xsplit", 2, &[1, 1])]);
            let lookup = chain_source(&table);
            let r = redeemable(&ps, |_| false, &WinnerSource::Chain(&lookup));
            assert_eq!(r.len(), 2, "0.5 is a payout, so both legs pay");
        }
    }
}
