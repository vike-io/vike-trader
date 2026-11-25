//! `positions` — the ONE reader of the Polymarket data-api's positions: CTF auto-redeem discovery
//! (docs/superpowers/specs/2026-07-11-ctf-redeem-design.md), the reconcile client
//! ([`crate::reconcile`]) and the operator's venue read (`reconcile::venue_read`) all
//! take their rows from here.
//!
//! # The route: `GET /v2/positions`
//!
//! Polymarket retired Data API v1 in November 2026: `/positions` answers HTTP 410 from everywhere
//! (measured through the Dublin tunnel, 2026-11-07). The contract is the venue's
//! `https://data-api.polymarket.com/v2/openapi.json` and `docs.polymarket.com/migrate/data-api-v1-to-v2`.
//!
//! | v1 `/positions` | `/v2/positions` | [`Position`] | what it is |
//! |---|---|---|---|
//! | `asset` | `token_id` | `asset` | the ERC-1155 outcome token |
//! | `conditionId` | `condition_id` | `condition_id` | the on-chain condition |
//! | `size` | `current_size` | `size` | shares held NOW. ⚠ NOT `total_size`, the LIFETIME bought shares |
//! | `avgPrice` | `average_price` | `average_price` | entry price per share, in USDC |
//! | `curPrice` | `current_price` | `current_price` | the indexer's mark per share, in USDC |
//! | `currentValue` | `current_value` | `current_value` | `current_size × current_price`, in USDC |
//! | `redeemable` | `redeemable` | `redeemable` | the market RESOLVED, losers included (below) |
//! | `negativeRisk` | `negative_risk` | `neg_risk` | |
//! | `outcomeIndex` | `outcome_index` | `outcome_index` | ⚠ `999` means unlabeled, read as `None` |
//! | `outcome`, `title` | `outcome`, `title` | `outcome`, `title` | words, as sent |
//!
//! **Measured, not only read in the contract (2026-11-07, through Dublin).** `current_size` equals the
//! CTF's own `balanceOf / 1e6` on both rows of the public sample address `0x…01`, whose `total_size` is
//! 0. The last v1 body of a real wallet, against its v2 rows: every field above equal on 4 of 4 rows,
//! while `total_size` agrees with the old `size` on only 3. On 902 open rows of a public wallet,
//! `entry_cost_usdc ≈ current_size × average_price` and `current_value = current_size × current_price`.
//!
//! **What the query must say, or the wallet reads as holding less** (`positions_query`):
//! - `include_archived=true`: the default hides archived markets (4 of 8244 rows on the measured
//!   wallet). The default `status=OPEN` is the superset the old route served: resolved holdings stay
//!   in it, losers included (`status` on the row says `REDEEMABLE`).
//! - `filter_amount=0`, the successor of `sizeThreshold=0`, which v2 ignores with a 200. ⚠ The venue's
//!   floor still held on four measured wallets (nothing under 0.1 share appeared at any value), so a
//!   holding below it (at most ten cents) stays invisible.
//! - `sort_by=TOKENS`: the default order is `current_value`, so a price tick can move a row across
//!   pages mid-walk and skip it. A holding's size moves only on the wallet's own fill.
//! - Every cursor page re-sends the anchor and filters (the cursor does not carry all of them); `limit`
//!   binds only the first page (`read_whole_wallet` walks; the measured 9-page wallet read 8244
//!   distinct rows).
//!
//! **No answer is ever an empty book.** A 410, any other non-2xx, a body that is no page, a row whose
//! holding cannot be read and a walk that does not end are all an `Err`: an empty or short list reads
//! as a wallet that sold, which `prune_flat` acts on and `crates/vike-execution/src/recon/diff.rs`'s `diff`
//! reports as `OrphanLocalPosition`. Only a row naming no token is skipped (it holds nothing a caller
//! could act on), as it always was.
//!
//! # ⚠ `redeemable: true` is NOT a winner flag
//!
//! This is the governing fact of this module, and it was learned the expensive way — twice, from
//! two independent live reads of this repo's own mainnet account (2026-07-23 and again 2026-08-02,
//! unchanged):
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
//!    [`crate::settlement::chain::ChainOracle::resolution`]. It is the contract state the redeem itself pays
//!    out of, so it cannot disagree with the money. It also expresses payouts no flag can: a SPLIT
//!    resolution (`[1,1]` over denominator 2) pays 0.5 to BOTH legs. Cost, per
//!    [`crate::settlement::chain::PolygonRpc::condition_resolution`]: an UNRESOLVED condition is exactly **one**
//!    `eth_call` — the `payoutDenominator` read returns 0 and short-circuits before
//!    `outcomeSlotCount`/`payoutNumerators` are asked for; a RESOLVED binary is
//!    `1 + 1 + 2 = four` (denominator, slot count, then one numerator per slot), paid **once**,
//!    because the oracle CACHES resolved verdicts (a resolved condition never un-resolves). So a
//!    settled watchlist costs zero calls per tick thereafter, and an unsettled one costs one call
//!    per open condition per pass.
//! 2. [`WinnerSource::CurrentPriceOnly`] — **an explicit opt-out, never the default.** The data-api's
//!    `current_price` convenience field, already in the positions payload, so it costs nothing. It
//!    agreed with the chain on 4/4 rows above — which is evidence, not proof. It is a *derived
//!    display* number computed by an off-chain indexer from a market's last state, not the payout
//!    the redeem contract reads: it can lag a fresh resolution, it is absent on a partial/degraded
//!    page (⇒ [`Payout::Unknown`], never a guessed 0), and no published contract binds it to
//!    `payoutNumerators`. **When it disagrees with the chain, the chain is right and `current_price` is
//!    wrong** — there is no case where a data-api convenience field overrules contract state (a
//!    pinned test states this: `when_current_price_disagrees_with_the_chain_the_chain_decides`).
//!    Choosing it means accepting one of two errors: a stale or absent price on a real winner
//!    delays a redeem by a tick (self-healing, see below), and a stale non-zero price on a loser
//!    costs one wasted relayer call. It exists for a host that genuinely cannot reach Polygon RPC,
//!    and a caller has to say its name to get it.
//!
//! # Every unknown fails CLOSED — including the one that selects the slot
//!
//! An RPC blip, a condition the chain says is not resolved yet, an **unreadable `outcome_index`**
//! (absent, non-numeric, out of `u32`, or the venue's `999` — see `parse_outcome_index`), an outcome
//! index outside the payout vector, and a missing `current_price` all yield [`Payout::Unknown`], which
//! [`redeemable`] excludes.
//!
//! `outcome_index` earns its own mention because under the chain-first design above it is strictly
//! MORE load-bearing than `current_price`: it chooses WHICH numerator is read. The pre-fix
//! `unwrap_or(0) as u32` answered every unreadable shape with slot `0` — and on the Solana row of
//! the live table above, resolved `[1, 0]` with slot **1** held, slot 0 is exactly the paying one.
//! So a degraded page did not merely lose information, it converted the held loser into a "winner",
//! submitted a relayer redeem worth nothing, and wrote a permanent at-most-once tombstone for it.
//! (The `as u32` cast made it worse: an index of `4294967296` WRAPPED to 0.) The field is now
//! `Option<u32>`, and an unreadable slot is `Unknown` under BOTH winner sources — see
//! [`payout_of`].
//!
//! Skipping is never a forfeit: discovery re-runs every tick and the position stays `redeemable`
//! until it is actually redeemed, so the only cost of a wrong `Unknown` is one tick of delay. The
//! opposite error — redeeming on a guess — burns a relayer call, and on the [`crate::settlement::resolve`] side
//! would fabricate realized PnL. This is the same refuse-to-guess rule
//! [`crate::settlement::resolve::ambiguous_conditions`] and [`crate::settlement::chain::join_settlement`] already follow.

use std::time::Duration;

use serde_json::Value;
use vike_bridge_core::json::get_str_borrowed;

use crate::refusals::PolymarketReadRefusal;
use crate::settlement::chain::ChainResolution;

/// The data-api route of a wallet's positions, on the venue's data-api row (`crate::config`'s
/// `data_api_base`).
const POSITIONS_PATH: &str = "/v2/positions";

/// The `outcome_index` the venue sends for an outcome it could not label: a sentinel, not a slot.
const UNLABELED_OUTCOME_INDEX: u32 = 999;

/// The rows [`read_whole_wallet`] asks of a page: the route's documented maximum.
const WALK_PAGE_ROWS: usize = 1000;

/// The pages one [`read_whole_wallet`] follows before it refuses: a hundred thousand holdings, far past
/// any wallet measured (the largest measured took 9). It also ends a cursor that never ends.
const MAX_WALK_PAGES: usize = 100;

/// One held position as the data-api reports it (the field map is this module's doc).
#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    /// Empty when the row names none (the wire requires it): [`PositionsClient::list`] drops such a
    /// row, because nothing can be redeemed or settled without one; a holding is still a holding to
    /// the reconcile client and the operator's read.
    pub condition_id: String,
    pub asset: String,
    /// `current_size`: the shares held NOW, never the row's lifetime `total_size`.
    pub size: f64,
    /// ⚠ A RESOLUTION flag, not a winner flag — see this module's doc. Every position of a resolved
    /// condition reports `true`, losers included.
    pub redeemable: bool,
    pub neg_risk: bool,
    /// Which outcome SLOT of the condition this row is (data-api `outcome_index`) — the index the
    /// chain's `payoutNumerators` vector is keyed by, and the slot a neg-risk redeem's AMOUNTS
    /// array is addressed to. `None` when the payload's value is absent, non-numeric, outside
    /// `u32`, or the venue's unlabeled sentinel.
    ///
    /// **`Option`, not a `0` fallback — and not a skipped row either.** See
    /// `parse_outcome_index` for why an unreadable slot must stay unreadable, and
    /// [`parse_positions`] for why the row is kept anyway.
    pub outcome_index: Option<u32>,
    pub title: String,
    /// The label of the held outcome (`Yes`, `Down`, …), as sent.
    pub outcome: String,
    /// data-api `average_price`: the entry price per share, in USDC. `None` when the row states none; a
    /// row may state `0.0`, which on a wallet given its tokens (no buy recorded) means no entry cost.
    pub average_price: Option<f64>,
    /// data-api `current_price` — the indexer's current mark for this outcome token (0 or 1 once the
    /// condition has resolved). `None` when the field is absent from the payload, which must stay
    /// distinguishable from a genuine `Some(0.0)`: absent is [`Payout::Unknown`] (skip and retry),
    /// zero is [`Payout::Loser`] (never redeem). Only consulted under
    /// [`WinnerSource::CurrentPriceOnly`].
    pub current_price: Option<f64>,
    /// data-api `current_value`: `current_size × current_price`, in USDC. `None` when not stated.
    pub current_value: Option<f64>,
}

/// One page of `GET /v2/positions`, parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionsPage {
    pub positions: Vec<Position>,
    /// The venue's cursor to the next page; `None` on the last one.
    pub next_cursor: Option<String>,
}

/// A number the row states, as a JSON number or a decimal string; `None` when it states none (the
/// venue's own rule: a missing or `null` number means unavailable, never zero).
fn stated(row: &Value, key: &str) -> Option<f64> {
    let v = row.get(key)?;
    let n = v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))?;
    n.is_finite().then_some(n)
}

/// Read the data-api `outcome_index` field. `None` — never a guessed `0` — for **every** shape that
/// is not an exact, in-range, non-negative integer: absent, `null`, a float, a bool, an object, a
/// non-numeric string, a negative, a value beyond `u32`, or the venue's `999` (could not be labeled).
///
/// # Why the number-OR-quoted-string tolerance is not invented leniency
///
/// It is this repo's already-pinned reading of **this very endpoint**: `recon_client`'s `number`/
/// `number_val` parsed the SAME v1 `/positions` rows and deliberately accepted both
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
/// addresses the redeem to the wrong slot. That is the same failure `current_price` is an
/// [`Option`] to avoid, one field down — and under the chain-first design `outcome_index` is
/// strictly MORE load-bearing than `current_price`, because it selects which numerator is read.
///
/// The `u32` bound is load-bearing too, not tidiness: the pre-fix `as u32` cast **wrapped**, so
/// an index of `4294967296` (2³²) truncated to exactly `0` and reached the relayer as a
/// "slot 0 winner". `u32::try_from` turns that into `None`.
fn parse_outcome_index(v: Option<&Value>) -> Option<u32> {
    let v = v?;
    // Number first, quoted decimal second — the `recon_client::number_val` shape, narrowed from f64 to
    // an exact integer so `"1.5"`/`1.5` cannot round into a slot.
    let n = v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))?;
    u32::try_from(n).ok().filter(|slot| *slot != UNLABELED_OUTCOME_INDEX)
}

/// Parse one page of `GET /v2/positions` (`{data: [..], pagination: {has_more, next_cursor, ..}}`).
/// `Err`, never an empty page, for a body that is no such page: an empty list would read as a wallet
/// that sold everything. A page saying more rows follow while naming no cursor is refused for the same
/// reason.
///
/// A row naming no `token_id` is skipped (tolerant, like the venue mappers): it names nothing a caller
/// could hold. **A row whose `current_size` cannot be read refuses the whole read**, naming the token:
/// reading it as `0` would report a held position as flat (a false `PositionDrift`), and skipping it
/// would report a false `OrphanLocalPosition`. A negative holding of a one-way token is unreadable too.
///
/// **An unreadable `outcome_index` does NOT skip the row** — it lands as `outcome_index: None` (see
/// `parse_outcome_index`). The two are treated differently on purpose: a row with an unreadable slot is
/// still a *held position*, and [`crate::settlement::resolve::ResolveWatchlist::prune_flat`] evicts every
/// watched token the snapshot no longer reports with `size > 0`. Skipping the row would therefore
/// make a degraded page look like a wallet that had SOLD the position and silently drop it from
/// settlement tracking — the exact "absent must stay distinguishable" failure this module refuses
/// elsewhere. Keeping it costs nothing: [`Payout::Unknown`] already excludes it from every redeem.
pub fn parse_positions(json: &Value) -> Result<PositionsPage, PolymarketReadRefusal> {
    let path = || POSITIONS_PATH.to_string();
    let list = json
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| PolymarketReadRefusal::NotAList { path: path() })?;
    let pagination = json
        .get("pagination")
        .filter(|p| p.is_object())
        .ok_or_else(|| PolymarketReadRefusal::NoPagination { path: path() })?;
    let next_cursor = pagination
        .get("next_cursor")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    if next_cursor.is_none() && pagination.get("has_more").and_then(Value::as_bool) == Some(true) {
        return Err(PolymarketReadRefusal::MoreWithoutCursor { path: path() });
    }
    let mut positions = Vec::with_capacity(list.len());
    for e in list {
        let asset = get_str_borrowed(e, "token_id").to_string();
        if asset.is_empty() {
            tracing::warn!(
                target: "vike_polymarket::positions",
                "a {POSITIONS_PATH} row names no token_id: skipped"
            );
            continue;
        }
        let Some(size) = stated(e, "current_size").filter(|size| *size >= 0.0) else {
            return Err(PolymarketReadRefusal::UnreadableSize { path: path(), asset });
        };
        positions.push(Position {
            condition_id: get_str_borrowed(e, "condition_id").to_string(),
            asset,
            size,
            redeemable: e.get("redeemable").and_then(Value::as_bool).unwrap_or(false),
            neg_risk: e.get("negative_risk").and_then(Value::as_bool).unwrap_or(false),
            // ABSENT/garbage stays absent — see `parse_outcome_index`. `unwrap_or(0)` here claimed
            // slot 0, which on a `[1, 0]` resolution reads Winner and redeems a loser.
            outcome_index: parse_outcome_index(e.get("outcome_index")),
            title: get_str_borrowed(e, "title").to_string(),
            outcome: get_str_borrowed(e, "outcome").to_string(),
            average_price: stated(e, "avg_price"),
            // ABSENT stays absent — `unwrap_or(0.0)` here would turn a degraded page into a wallet
            // full of "losers", which is exactly the kind of silent guess this module refuses.
            current_price: stated(e, "current_price"),
            current_value: stated(e, "current_value"),
        });
    }
    Ok(PositionsPage { positions, next_cursor })
}

/// The query of one page: the widest list the route offers (this module's doc says why each part is
/// there). A cursor page re-sends the anchor and filters; `limit` binds only the first page.
fn positions_query(funder: &str, limit: usize, cursor: Option<&str>) -> String {
    let anchor = format!("user={funder}&filter_amount=0&include_archived=true&sort_by=TOKENS");
    match cursor {
        Some(cursor) => {
            format!("{anchor}&cursor={}", vike_utility::percent::percent_encode(cursor))
        }
        None => format!("{anchor}&limit={limit}"),
    }
}

/// `GET /v2/positions` for `funder` on `data_api`, one page, through the proxy-aware agent
/// ([`crate::transport::agent_with_timeout`]): the body, or the failure in words. Public and keyless, so it
/// carries no credential. The address goes into a query string, so only a plain `0x` address is asked
/// for. A failure says the status head only, never the body (the rule of a signed read); a 410 says
/// the venue retired the path ([`PolymarketReadRefusal::Retired`]).
pub(crate) fn fetch_positions_page(
    data_api: &str,
    funder: &str,
    limit: usize,
    cursor: Option<&str>,
    timeout: Duration,
) -> Result<Value, PolymarketReadRefusal> {
    let is_address = funder.len() == 42
        && funder.starts_with("0x")
        && funder[2..].bytes().all(|b| b.is_ascii_hexdigit());
    if !is_address {
        return Err(PolymarketReadRefusal::FunderNotAnAddress);
    }
    let path = || POSITIONS_PATH.to_string();
    let url = format!("{data_api}{POSITIONS_PATH}?{}", positions_query(funder, limit, cursor));
    let network =
        |e: ureq::Error| PolymarketReadRefusal::Network { path: path(), why: e.to_string() };
    let mut response =
        crate::transport::agent_with_timeout(timeout).get(&url).call().map_err(network)?;
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().map_err(network)?;
    if status == 410 {
        return Err(PolymarketReadRefusal::Retired { path: path() });
    }
    if !(200..300).contains(&status) {
        return Err(PolymarketReadRefusal::HttpStatus { path: path(), status });
    }
    serde_json::from_str(&text).map_err(|_| PolymarketReadRefusal::NotJson { path: path(), status })
}

/// Every position `funder` holds, walking the venue's cursor to its end: a wallet of more than one page
/// does not read as holding less. All or nothing: a page that fails fails the read, and the next pass or
/// tick asks again; a partial list would be the short book this module refuses.
///
/// shortcut: a fill landing mid-walk can still move a row across a page boundary (and cost one read of
/// that row, cured by the next one); only a wallet of more than 1000 rows can see it, so no snapshot
/// logic was built. Upgrade if such a wallet trades here.
pub(crate) fn read_whole_wallet(
    data_api: &str,
    funder: &str,
    timeout: Duration,
) -> Result<Vec<Position>, PolymarketReadRefusal> {
    let mut held = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_WALK_PAGES {
        let body =
            fetch_positions_page(data_api, funder, WALK_PAGE_ROWS, cursor.as_deref(), timeout)?;
        let page = parse_positions(&body)?;
        held.extend(page.positions);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok(held),
        }
    }
    Err(PolymarketReadRefusal::WalkUnended {
        path: POSITIONS_PATH.to_string(),
        pages: MAX_WALK_PAGES,
    })
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
    /// vector, or an absent `current_price`. Treated as "not now" — [`redeemable`] excludes it and the
    /// next tick asks again.
    Unknown,
}

/// Where [`payout_of`] reads winner truth from. **The default is [`Chain`](WinnerSource::Chain);
/// [`CurrentPriceOnly`](WinnerSource::CurrentPriceOnly) is an explicit opt-out.** See this module's doc for
/// the full argument.
pub enum WinnerSource<'a> {
    /// AUTHORITATIVE: the CTF's own payout mappings, via a `conditionId -> resolution` lookup
    /// (normally [`crate::settlement::chain::ChainOracle::resolution`], which caches and returns `None` on a
    /// transport failure rather than asserting "unresolved").
    Chain(&'a dyn Fn(&str) -> Option<ChainResolution>),
    /// OPT-OUT: the data-api's `current_price`. Network-free, and only as good as the indexer.
    CurrentPriceOnly,
}

/// One position's payout verdict under `source`. Pure: the `Chain` variant does its I/O inside the
/// caller-supplied closure, so the whole decision table is testable offline.
///
/// **An unreadable `outcome_index` is [`Payout::Unknown`] under BOTH sources**, including
/// [`WinnerSource::CurrentPriceOnly`], which does not itself need the slot to tell a winner from a
/// loser. That is deliberate: [`Payout::Winner`] is not an opinion, it is a licence to submit a
/// relayer redeem — and a neg-risk redeem's AMOUNTS array is addressed BY that slot
/// (`auto_redeem::neg_risk_amounts`). Knowing a position pays is useless if the redeem cannot be
/// pointed at the right leg, so the gate is uniform rather than per-source.
pub fn payout_of(p: &Position, source: &WinnerSource<'_>) -> Payout {
    // No readable slot ⇒ no verdict, under any source. Fails closed before the source is consulted.
    let Some(index) = p.outcome_index else { return Payout::Unknown };
    match source {
        WinnerSource::Chain(lookup) => match lookup(&p.condition_id) {
            // `payout_for_index` is `None` for an unresolved condition or an out-of-range slot;
            // both are genuinely unknown, never "worthless".
            Some(r) => match r.payout_for_index(index) {
                Some(x) if x > 0.0 => Payout::Winner,
                Some(_) => Payout::Loser,
                None => Payout::Unknown,
            },
            None => Payout::Unknown,
        },
        // A resolved market marks 0 or 1, so `> 0.0` is the whole test. A dust non-zero mark (never
        // observed) would read Winner and cost one wasted relayer call — the cheap direction.
        WinnerSource::CurrentPriceOnly => match p.current_price {
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
/// positions but none of them pays" needs both sets. `tests/polymarket_smokes/redeem_smoke.rs` uses it to FAIL LOUDLY
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
/// It exists because [`crate::settlement::auto_redeem::RedeemDependencies`] owns the poller's whole outside world behind
/// one trait — including which winner source is configured — so the poller injects
/// `dependencies.payout(p)` here rather than re-deriving a `WinnerSource` it does not hold. That also makes
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

/// Live discovery: every page of `GET {data_api}/v2/positions` for the wallet — reachable via the
/// Dublin tunnel like the other Polymarket reads (`transport::agent_with_timeout` routes through the SOCKS
/// proxy when enabled).
pub struct PositionsClient;

impl PositionsClient {
    /// Every holding of `proxy_address` from the data-api at `data_api` (the venue's data-api row).
    pub fn list(data_api: &str, proxy_address: &str) -> Result<Vec<Position>, String> {
        let mut held = read_whole_wallet(data_api, proxy_address, crate::transport::REST_TIMEOUT)
            .map_err(|refusal| refusal.to_string())?;
        // A row naming no condition cannot be priced, redeemed or settled.
        held.retain(|p| !p.condition_id.is_empty());
        Ok(held)
    }
}

#[path = "positions_tests.rs"]
#[cfg(test)]
mod positions_tests;
