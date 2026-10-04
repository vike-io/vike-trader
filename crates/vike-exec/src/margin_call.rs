//! Margin-call model — LEAN `DefaultMarginCallModel` semantics over the vike `Account`.
//! Rust-native (no Python twin). Twin, verified from source 2026-07-07:
//! `Common/Securities/DefaultMarginCallModel.cs` — warning at margin-remaining ≤ 5% of
//! equity; liquidation ONLY when remaining ≤ 0 AND margin used > equity·(1+buffer);
//! biggest LOSERS liquidated first; liquidate only the excess and stop as soon as healthy.
//!
//! PURE: computes intents; the caller (vike-core watchdog, opt-in via `CoreConfig`)
//! mints coids and submits through the normal gate path (`OrderDenied` on veto — a margin
//! call can never bypass the RiskGate). Unmarked positions contribute no margin and are
//! never liquidated (LEAN skips groups it cannot price).

use crate::account::Account;

#[derive(Debug, Clone, Copy)]
pub struct MarginCallConfig {
    /// maintenance-margin fraction (LEAN `1/leverage` shape), e.g. 0.05
    pub mm_requirement: f64,
    /// warning when margin remaining ≤ equity · this (LEAN hardcodes 0.05)
    pub warn_fraction: f64,
    /// liquidate only when margin used > equity · (1 + buffer) (LEAN default 0.10)
    pub buffer: f64,
}

impl Default for MarginCallConfig {
    fn default() -> Self {
        MarginCallConfig { mm_requirement: 0.05, warn_fraction: 0.05, buffer: 0.10 }
    }
}

/// A position-reducing market order the caller should submit (gate-checked, reduce_only).
#[derive(Debug, Clone, PartialEq)]
pub struct LiquidationIntent {
    pub venue: String,
    pub symbol: String,
    pub position_side: String,
    /// closing side: opposite the position sign
    pub side: i32,
    pub qty: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarginCall {
    Healthy,
    /// remaining ≤ equity · warn_fraction but not liquidatable yet
    Warning {
        margin_used: f64,
        margin_remaining: f64,
    },
    /// remaining ≤ 0 and used > equity · (1+buffer): reduce these, losers first
    Liquidate(Vec<LiquidationIntent>),
}

/// One sweep of the scope-parameterized liquidation law over the account at current marks
/// (`vike_model::money::liquidation` — ONE law, the pool is the parameter).
///
/// The account's positions are partitioned by [`vike_model::MarginMode`]
/// (`vike_model::partition_pools`):
/// - **Cross** positions (the default — every position today) share ONE pool: the LEAN
///   `DefaultMarginCallModel` workflow, unchanged — losers first, excess only, stop when
///   healthy. A cross-only account is BYTE-IDENTICAL to the pre-partition sweep (that is the
///   compat pin; see `cross_only_verdicts_byte_identical_to_legacy` below).
/// - **Isolated** positions each form their OWN pool (`isolated_margin` wallet + own uPnL);
///   a breach (zero buffer — no LEAN grace line on a walled-off wallet) closes THAT position
///   only, full size, and can never drag the cross pool: its wallet AND uPnL are subtracted
///   from the cross pool's equity. An isolated position with no reported wallet, or no mark,
///   is unpriceable → skipped (the LEAN skip). Isolated pools have no Warning arm.
/// - **Cash** positions never liquidate (fully funded — the pool that structurally cannot
///   breach); their value simply remains account equity backing the cross pool.
///
/// MAINTENANCE RATE — one source: `cfg.mm_requirement` (the operator config) prices every
/// pool. A per-symbol venue-reported rate would take precedence when present, but no venue
/// adapter parses one today — see the precedence note on `vike_model::pool_breached`.
///
/// PRICE BASIS: this legacy-signature entry prices every position at `Account.marks` (the
/// pre-resolver basis) and exists for callers that have no price board (tests, offline tools).
/// The LIVE watchdog calls [`check_margin_call_priced`] with the PR-1 resolver closure instead,
/// so margin-used shares the price basis of the resolver-priced `equity` it is compared against
/// — a stale mark can no longer overstate margin against a fresh-quote equity (or vice versa).
pub fn check_margin_call(account: &Account, equity: f64, cfg: &MarginCallConfig) -> MarginCall {
    check_margin_call_priced(account, equity, cfg, |(v, s, _side), _p| account.mark_of(v, s))
}

/// The price-generalized margin-call law — ONE law, the valuation price is the parameter
/// (`price_of(key, entry)`; `None` = unpriceable → the LEAN skip everywhere a mark-less
/// position was skipped before). EVERY price read in the sweep goes through `price_of` — the
/// cross margin-used fold, each pool's unrealized PnL, the isolated maintenance line, and the
/// cross candidates — so a single price basis judges numerator and denominator alike.
/// With `price_of = marks lookup` this is bit-identical to the pre-generalization sweep
/// ([`check_margin_call`] delegates here; the `cross_only_verdicts_byte_identical_to_legacy`
/// pin below discriminates a drift).
pub fn check_margin_call_priced(
    account: &Account,
    equity: f64,
    cfg: &MarginCallConfig,
    price_of: impl Fn(&crate::account::PositionKey, &crate::account::PositionEntry) -> Option<f64>,
) -> MarginCall {
    // THE pool partition (position insertion order preserved).
    let pools = vike_model::partition_pools(account.positions.values().map(|p| p.margin_mode));

    // Cross-pool maintenance = Σ over open CROSS positions |size|·px·mult·mm_req
    // (unpriceable → 0). Same shared fold (`Account::margin_in_use_priced`) as the
    // gate/snapshot, but the RATE POLICY is intentionally MAINTENANCE margin
    // (`cfg.mm_requirement`), applied flatly — the watchdog judges health on maintenance,
    // not the initial margin the admitting gate uses. The fold is shared; the rate differs
    // by design. Isolated/Cash positions return None → excluded (their own pools / never).
    let margin_used = account.margin_in_use_priced(&price_of, |_k, p| {
        p.margin_mode.is_cross().then_some(cfg.mm_requirement)
    });

    // Isolated arms: judge each walled-off pool, and remove its equity (wallet + own uPnL)
    // from the cross pool — isolated positions no longer share account collateral. With no
    // isolated positions this loop is empty and `cross_equity == equity` bit-identically.
    //
    // KNOWN VENUE ASYMMETRY (mode-without-wallet, today: Bybit): a venue can report a
    // position Isolated while surfacing NO per-position wallet (Bybit v5's parse deliberately
    // sets `isolated_margin: None` — `positionIM` is plain initial margin, present for cross
    // too, and `positionBalance` is undocumented for UTA, so labeling either "the wallet"
    // would be a guess; still unverifiable from the crate's fixtures/docs as of 2026-07-19).
    // For such a position the `unwrap_or(0.0)` below subtracts only its uPnL, the `else`
    // skips its local isolated judgment (LEAN skip — never a spurious close), and the UNKNOWN
    // wallet stays folded inside the account `balance` → the cross pool's equity is
    // OVERSTATED by that wallet's size, so a genuine cross breach is judged LATE here. The
    // venue itself still liquidates the isolated position (its engine holds the real wallet);
    // the exposure is a late LOCAL cross verdict, not an unliquidated isolated position. The
    // eventual fix is parsing the venue's real per-position wallet carrier into
    // `isolated_margin` — done only once its field semantics are verifiable, never guessed.
    //
    // LIVE PROBE 2026-07-19 (vike-bybit `tests/bybit_isolated_wallet_probe.rs`, reproducible):
    // verification is VENUE-BLOCKED on the demo account — both isolation paths refused
    // (`/v5/position/switch-isolated` retCode 10032 "Demo trading are not supported.";
    // `/v5/account/set-margin-mode ISOLATED_MARGIN` retCode 110073 "Set margin mode failed"),
    // and the CROSS control position's raw row carried `positionBalance:"0"` — populated for
    // cross too, so populated-ness can never identify an isolated wallet. Bybit's v5 docs now
    // deprecate BOTH `positionBalance` ("can refer to positionIM") AND `tradeMode` (always `0`
    // on UTA — margin mode moved to `/v5/account/account-info`), so on UTA payloads
    // `parse_trade_mode` never yields Isolated and this arm never fires for bybit; the
    // exposure above is confined to classic-account payloads, which carry `tradeMode:1` but
    // still no verifiable wallet field. `isolated_margin: None` stays the pinned parse.
    //
    // INVARIANT (the other side of the same subtraction): the venue balance folded into the
    // account — `ReconcileSnapshot.balance` — MUST be the TOTAL wallet including isolated
    // allocations (Binance's `balance` and Bybit's `walletBalance` both are); a future venue
    // feeding a cross-only balance here would make this arm silently DOUBLE-SUBTRACT every
    // reported isolated wallet.
    let mut cross_equity = equity;
    let mut iso_intents: Vec<LiquidationIntent> = Vec::new();
    for &i in &pools.isolated {
        let (key, p) = account.positions.get_index(i).expect("partition index");
        let (v, s, side) = key;
        if p.size == 0.0 {
            continue;
        }
        // Priced through the ONE `price_of` basis; unpriceable → 0.0 uPnL, the exact
        // silent-zero `unrealized_pnl` gave an unmarked position.
        let px = price_of(key, p);
        let upnl =
            px.map(|px| account.unrealized_at(s, *side, p.size, p.avg_px, px)).unwrap_or(0.0);
        cross_equity -= p.isolated_margin.unwrap_or(0.0) + upnl;
        let (Some(wallet), Some(px)) = (p.isolated_margin, px) else {
            continue; // no wallet reported / unpriceable → cannot judge (LEAN skip)
        };
        let maint = p.size.abs() * px * account.multiplier_of(s) * cfg.mm_requirement;
        if vike_model::pool_breached(wallet + upnl, maint, 0.0) {
            iso_intents.push(LiquidationIntent {
                venue: v.to_string(),
                symbol: s.to_string(),
                position_side: side.to_string(),
                side: vike_model::closing_side(p.size),
                qty: p.size.abs(), // full close — the loss is capped at the isolated wallet
            });
        }
    }

    // Cross arm: the ONE law with the LEAN buffer, then the losers-first excess-only plan.
    let mut intents: Vec<LiquidationIntent> = Vec::new();
    if vike_model::pool_breached(cross_equity, margin_used, cfg.buffer) {
        // candidates in position insertion order (the plan's stable sort keeps ties there)
        let mut keys: Vec<(String, String, String, f64)> = Vec::new();
        let mut candidates: Vec<vike_model::LiqCandidate> = Vec::new();
        for &i in &pools.cross {
            let (key, p) = account.positions.get_index(i).expect("partition index");
            let (v, s, side) = key;
            if p.size == 0.0 {
                continue;
            }
            let Some(px) = price_of(key, p) else {
                continue; // cannot price → cannot liquidate (LEAN skip)
            };
            let upnl = account.unrealized_at(s, *side, p.size, p.avg_px, px);
            candidates.push(vike_model::LiqCandidate {
                id: keys.len(),
                size: p.size,
                mark: px,
                mult: account.multiplier_of(s),
                upnl,
            });
            keys.push((v.to_string(), s.to_string(), side.to_string(), p.size));
        }
        let excess = margin_used - cross_equity;
        for (id, qty) in vike_model::cross_liquidation_plan(candidates, excess, cfg.mm_requirement)
        {
            let (v, s, side, size) = &keys[id];
            intents.push(LiquidationIntent {
                venue: v.clone(),
                symbol: s.clone(),
                position_side: side.clone(),
                side: vike_model::closing_side(*size),
                qty,
            });
        }
    }
    // Cross plan first, then isolated closes — one deterministic release order for the
    // watchdog's journal-then-submit loop.
    intents.extend(iso_intents);
    if !intents.is_empty() {
        return MarginCall::Liquidate(intents);
    }
    // Warning arm (cross pool only — an isolated pool either breaches or it doesn't).
    if margin_used > 0.0 {
        let remaining = cross_equity - margin_used;
        if remaining <= cross_equity * cfg.warn_fraction {
            return MarginCall::Warning { margin_used, margin_remaining: remaining };
        }
    }
    MarginCall::Healthy
}

#[path = "margin_call_tests.rs"]
#[cfg(test)]
mod margin_call_tests;
