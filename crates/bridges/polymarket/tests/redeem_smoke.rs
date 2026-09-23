//! LIVE Polymarket CTF-redeem smoke — real-money Polygon **mainnet** gasless relayer redemption.
//! `#[ignore]`d and DOUBLE-GATED: self-skips unless `POLY_PRIVATE_KEY` (+ the relayer key/address)
//! are in the workspace `.env`, AND — on top of that — self-skips unless `POLY_REDEEM_SMOKE=1` is
//! explicitly set (this places a REAL on-chain redemption; presence of creds alone is not enough).
//! The data-api and relayer are geo-blocked, so run WITH the Dublin SOCKS proxy (arbdub):
//!
//! ```sh
//! POLY_SOCKS_PROXY=socks5://127.0.0.1:1080 POLY_REDEEM_SMOKE=1 \
//!   cargo test -p vike-polymarket --features polymarket --test redeem_smoke \
//!   -- --ignored --nocapture
//! ```
//!
//! This is the **arbdub live-verification gate** that must pass before `POLY_AUTO_REDEEM` is ever
//! set on a running poller: `redeem_relayer` pinned its wire format from reading the reference
//! `rs-builder-relayer-client` SDK source, not from a live round-trip — the nonce/deadline batch
//! semantics and the exact `RelayerTransactionResponse` field names are marked ASSUMED there.
//! Running this smoke against a real account with real redeemable positions — binary AND neg-risk —
//! is what confirms (or corrects) those assumptions before the unattended auto-redeem poller is ever
//! enabled for real money.
//!
//! ⚠ **What this smoke no longer has to prove: the V2 neg-risk ABI.** The neg-risk plan (Tasks 1-4)
//! pinned the NegRiskAdapter `redeemPositions(bytes32,uint256[])` selector (`0xdbeccb23`) + its
//! per-slot-AMOUNTS array from V1 contract source and left the V2 shape ASSUMED. That assumption was
//! WRONG and was settled STATICALLY on 2026-08-02 against the VERIFIED `NegRiskCtfCollateralAdapter`
//! ABI + source — no position and no round-trip needed: on [`Era::V2`] a neg-risk redeem is the
//! 4-arg CTF encoding (`0x01b7037c`), byte-identical to a binary redeem, differing ONLY by target,
//! and the `uint256[]` is IGNORED. See `redeem.rs`'s module doc. What the neg-risk leg here still
//! proves is everything a static read cannot: that the RELAYER accepts a batch aimed at the
//! neg-risk target, that it mines, and that the position actually settles.
//!
//! Behavior when fully enabled: load creds, resolve the proxy/deposit-wallet address, list positions
//! via `PositionsClient::list`, and filter to the positions that actually PAY (empty exclude, no
//! ledger — a throwaway `RedeemLedger` so nothing here has a persistent side effect beyond the
//! on-chain redeem itself). If a **binary** winner exists, `submit_redeem` it with
//! `RedeemKind::Binary` and assert the relayer returned a result (a `tx_hash`, or at least a
//! non-empty raw body — the "response fields" unknown this smoke is meant to resolve). If a
//! **neg-risk** winner (`Position.neg_risk`) also exists, `submit_redeem` it with
//! `RedeemKind::NegRisk(amounts)` — the per-slot amounts derived the same way
//! `auto_redeem.rs::neg_risk_amounts` derives them (base-unit size at the position's outcome slot,
//! 0 at the other; duplicated here rather than exported, since this is the only external caller;
//! ⚠ INERT on V2, which ignores the array — kept so this smoke exercises the poller's exact call
//! shape) — and assert the relayer returned a result too. Either leg is independently optional: if no
//! WINNING position of that kind exists on the account, that is NOT a failure — it prints that and
//! moves on.
//!
//! **Since the redeem-confirmation fix**, each leg mirrors the poller's real two-phase order —
//! `RedeemLedger::begin` (the write-ahead record) BEFORE `submit_redeem`, `attach_tx` after — and
//! then prints the ON-CHAIN confirmation verdict (`report_confirmation`). A relayer 2xx no longer
//! settles anything anywhere, so this smoke no longer pretends it does; see that helper's doc for
//! the two live unknowns its output is meant to resolve.
//!
//! # ⚠ It selects a WINNER, and it FAILS if it cannot find one
//!
//! This smoke used to pick `candidates.iter().find(|p| !p.neg_risk)` out of the set filtered on the
//! data-api's `redeemable` flag. That flag is set on every position of a RESOLVED condition, losers
//! included (`positions.rs`'s module doc has the measured table), and on this repo's own mainnet
//! wallet the data-api order is Solana(loser), Doge(loser), Doge(loser), BNB(winner) — so the `find`
//! selected the **Solana loser**, submitted a redeem that pays **$0**, and reported success. It
//! still exercised nonce/deadline/headers/response-field parsing, which is most of the job, but it
//! proved nothing whatsoever about payout correctness — and the one $5 winner, last in the list, was
//! unreachable.
//!
//! Two changes close that:
//!   1. the candidate set is filtered by the AUTHORITATIVE on-chain payout
//!      (`WinnerSource::Chain` over a `ChainOracle`, the same source the poller uses), so a `find`
//!      cannot land on a loser; and
//!   2. if the wallet HAS resolved positions but none of them pays, the test **panics** instead of
//!      printing "nothing to do". A smoke that quietly proves less than it claims is the exact
//!      failure mode being fixed, so the only silent path left is a wallet with no resolved
//!      positions at all.
//!
//! It prints the full per-position verdict table before acting, so the operator can see exactly what
//! the chain said about every row.

#![cfg(feature = "polymarket")]

use vike_bridge_core::credentials::load_workspace_dotenv_from;
use vike_polymarket::{
    ChainOracle, ChainRedeemConfirmer, Era, PolymarketCreds, Position, PositionsClient, RedeemKind,
    WinnerSource, payout_of, redeemable, resolved_candidates, submit_redeem,
};

/// USDC-collateralized CTF conditional tokens use 6 decimals — same scale as
/// `auto_redeem.rs::CTF_TOKEN_DECIMALS_SCALE`.
const CTF_TOKEN_DECIMALS_SCALE: f64 = 1_000_000.0;

/// Duplicate of `auto_redeem.rs::neg_risk_amounts` (private there — not exported just for this
/// smoke, per the neg-risk plan's Task 5 brief). Derives the NegRiskAdapter `redeemPositions`
/// per-slot AMOUNTS array from a redeemable neg-risk `Position`: the base-unit balance
/// (`round(size * 1e6)`) at the position's `outcome_index` slot (clamped to 0/1), `0` at the other.
/// Panic-free for garbage sizes (NaN/negative saturate to `0`, a safe no-op amount), and an
/// UNREADABLE slot (`outcome_index: None`) takes the same all-zero exit rather than inventing one.
fn neg_risk_amounts_for_smoke(p: &Position) -> Vec<u128> {
    let base_units = (p.size * CTF_TOKEN_DECIMALS_SCALE).round();
    let amount = if base_units.is_finite() && base_units > 0.0 { base_units as u128 } else { 0 };
    let mut amounts = vec![0u128; 2];
    let Some(slot) = p.outcome_index.map(|i| (i as usize).min(1)) else { return amounts };
    amounts[slot] = amount;
    amounts
}

/// Print what the ON-CHAIN confirmation path sees right after a submit — the second half of this
/// smoke's job since the redeem-confirmation fix.
///
/// Two live unknowns get resolved by reading this output:
///   1. **Does the relayer return a `transactionHash` on `/submit` at all?** The reference SDK's
///      response is `{"transactionID":"…","state":"NEW"}` — `transactionHash` is documented but
///      unobserved. No hash means the poller has nothing to look up and falls back to its
///      submit-window timeout, so this is worth knowing before `POLY_AUTO_REDEEM` is enabled.
///   2. **Does `eth_getTransactionReceipt` resolve that hash?** Immediately after a submit the
///      expected verdict is `Unmined` (the batch is still queued) — that is a PASS, not a failure.
///      Re-run this helper's query manually a minute later to watch it become `Settled`.
///
/// Log-only and non-blocking on purpose: a real confirmation takes ~a minute, which is the poller's
/// job across ticks, not a smoke's job to sit and wait for.
fn report_confirmation(leg: &str, condition_id: &str, tx_hash: Option<&str>) {
    let Some(tx) = tx_hash else {
        tracing::warn!(
            target: "vike_polymarket",
            "{leg} {condition_id}: relayer returned NO transactionHash — the poller will fall back to its submit-window timeout. RECORD THIS."
        );
        return;
    };
    let verdict = ChainRedeemConfirmer::from_env().confirm(condition_id, tx);
    tracing::info!(
        target: "vike_polymarket",
        "{leg} {condition_id}: on-chain confirmation read for tx {tx} -> {verdict:?} (Unmined right after submit is EXPECTED)"
    );
}

/// Load creds + the proxy/deposit-wallet address, or `None` to self-skip (the credential gate).
/// Mirrors `polymarket_user_smoke.rs::load_live_ctx`'s manual `vars.get(...)` pattern — the redeem
/// path needs only the L1 key + the relayer key/address (no L2 CLOB auth: `submit_redeem` and
/// `PositionsClient::list` are unauthenticated/relayer-only, unlike order placement).
fn load_redeem_ctx() -> Option<(PolymarketCreds, String)> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let private_key = vars.get("POLY_PRIVATE_KEY").filter(|s| !s.is_empty())?.clone();
    let relayer_key = vars.get("POLY_RELAYER_API_KEY").filter(|s| !s.is_empty())?.clone();
    let relayer_address =
        vars.get("POLY_RELAYER_API_KEY_ADDRESS").filter(|s| !s.is_empty())?.clone();
    let address = vars.get("POLY_ADDRESS").cloned().unwrap_or_default();
    // proxy/deposit-wallet: the account's funder, same default fallback as polymarket_user_smoke.rs.
    let proxy = vars
        .get("POLY_FUNDER")
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| "0x107C01D04Fd68557ACd52E89dD01972b22803aD5".to_string());
    let creds = PolymarketCreds {
        private_key,
        address,
        relayer_key,
        relayer_address,
        ..Default::default()
    };
    Some((creds, proxy))
}

#[test]
#[ignore = "LIVE real-money mainnet CTF redeem — opt-in POLY_REDEEM_SMOKE=1; run manually via arbdub"]
fn ctf_redeem_smoke() {
    vike_log::test_init();

    let Some((creds, proxy)) = load_redeem_ctx() else {
        tracing::warn!(target: "vike_polymarket", "SKIP: POLY creds (private key / relayer key / relayer address) absent");
        return;
    };

    if std::env::var("POLY_REDEEM_SMOKE").ok().as_deref() != Some("1") {
        tracing::warn!(
            target: "vike_polymarket",
            "SKIP: creds present but POLY_REDEEM_SMOKE != 1 — this places a REAL on-chain redemption, so it requires an explicit opt-in on top of credentials"
        );
        return;
    }

    tracing::warn!(target: "vike_polymarket", "REAL-MONEY CTF redeem smoke: proxy={proxy}");

    let positions = PositionsClient::list(&proxy).expect("PositionsClient::list should succeed");
    // Empty exclude + a throwaway ledger (no persisted state): this smoke doesn't own the poller's
    // idempotency ledger, it just needs ONE WINNING position of each kind to exercise the real
    // relayer round-trip for both contract paths.
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = vike_polymarket::RedeemLedger::open(dir.path().join("smoke-ledger.jsonl"));

    // The AUTHORITATIVE winner source — the CTF's own payoutNumerators, exactly what `ProdDeps`
    // uses. Reads only (`eth_call`); never signs.
    let oracle = ChainOracle::from_env();
    let lookup = |cid: &str| oracle.resolution(cid);
    let winner_source = WinnerSource::Chain(&lookup);

    // `is_settled` — the ON-CHAIN-PROVEN filter. A throwaway ledger has nothing settled, so this is
    // the empty filter here; it is spelled out rather than `|_| false` so the smoke exercises the
    // same discovery predicate the poller uses.
    let resolved = resolved_candidates(&positions, |cid| ledger.is_settled(cid));
    let candidates = redeemable(&positions, |cid| ledger.is_settled(cid), &winner_source);

    // The verdict table: what the flag said vs. what the chain said, for every held position.
    for p in &positions {
        tracing::info!(
            target: "vike_polymarket",
            "position condition_id={} outIdx={:?} size={} neg_risk={} redeemable={} curPrice={:?} chain_payout={:?} title={:?}",
            p.condition_id, p.outcome_index, p.size, p.neg_risk, p.redeemable, p.cur_price,
            payout_of(p, &winner_source), p.title
        );
    }
    tracing::warn!(
        target: "vike_polymarket",
        "{} held / {} flagged redeemable / {} actually pay — `redeemable: true` is a RESOLUTION flag, not a winner flag",
        positions.len(), resolved.len(), candidates.len()
    );

    // ⚠ FAIL LOUDLY, never skip: resolved positions but no winner among them means either the wallet
    // holds only losers (then this smoke CANNOT prove payout correctness and must not pretend to) or
    // the chain read is broken (a wrong RPC, a geo-blocked route). Both are results worth a red test.
    assert!(
        resolved.is_empty() || !candidates.is_empty(),
        "{} position(s) report redeemable: true but the chain says NONE of them pays. \
         This smoke exists to prove a REAL payout; redeeming a loser proves only that the relayer \
         accepts a request. Fund the wallet with a winning position (or fix the RPC route) and re-run. \
         Resolved conditionIds: {:?}",
        resolved.len(),
        resolved.iter().map(|p| p.condition_id.as_str()).collect::<Vec<_>>()
    );

    // --- binary leg: CTF redeemPositions(address,bytes32,bytes32,uint256[]) ---
    // `candidates` is winner-filtered, so this `find` cannot select a worthless position.
    match candidates.iter().find(|p| !p.neg_risk) {
        Some(pos) => {
            tracing::info!(
                target: "vike_polymarket",
                "redeeming BINARY WINNER condition_id={} title={:?} size={} (chain payout {:?})",
                pos.condition_id, pos.title, pos.size, payout_of(pos, &winner_source)
            );
            // Era::V2 (pUSD): current activity routes to the CtfCollateralAdapter — the V1 relayer
            // path was retired 2026-07-17. See `vike_polymarket::Era`.
            // Write-ahead FIRST (the poller's order), then submit. `mark`-on-2xx is gone: a relayer
            // 2xx records only an in-flight submit now.
            ledger.begin(&pos.condition_id, 0);
            let result =
                submit_redeem(&creds, &proxy, &pos.condition_id, &RedeemKind::Binary, Era::V2)
                    .expect("submit_redeem (binary) should succeed against the live relayer");
            if let Some(tx) = result.tx_hash.as_deref() {
                ledger.attach_tx(&pos.condition_id, tx);
            }
            report_confirmation("BINARY", &pos.condition_id, result.tx_hash.as_deref());

            assert!(
                result.tx_hash.is_some() || !result.raw.trim().is_empty(),
                "relayer must return a tx_hash or at least a parseable non-empty raw response (binary)"
            );
            tracing::info!(
                target: "vike_polymarket",
                "✅ SUBMITTED BINARY condition_id={} tx_hash={:?} raw={} (submitted, NOT yet settled — see the confirmation line above)", pos.condition_id, result.tx_hash, result.raw
            );
        }
        None => {
            // Not a failure on its own — the loud "resolved but nothing pays" case already
            // panicked above, so reaching here means the only winners are neg-risk ones.
            tracing::info!(
                target: "vike_polymarket",
                "no WINNING binary position found on {proxy} — skipping the binary leg"
            );
        }
    }

    // --- neg-risk leg: NegRiskAdapter redeemPositions(bytes32,uint256[]) with per-slot amounts ---
    match candidates.iter().find(|p| p.neg_risk) {
        Some(pos) => {
            let amounts = neg_risk_amounts_for_smoke(pos);
            tracing::info!(
                target: "vike_polymarket",
                "redeeming NEG-RISK WINNER condition_id={} title={:?} size={} amounts={:?} (chain payout {:?})",
                pos.condition_id, pos.title, pos.size, amounts, payout_of(pos, &winner_source)
            );
            ledger.begin(&pos.condition_id, 0);
            let result = submit_redeem(
                &creds,
                &proxy,
                &pos.condition_id,
                &RedeemKind::NegRisk(amounts),
                Era::V2,
            )
            .expect("submit_redeem (neg-risk) should succeed against the live relayer");
            if let Some(tx) = result.tx_hash.as_deref() {
                ledger.attach_tx(&pos.condition_id, tx);
            }
            report_confirmation("NEG-RISK", &pos.condition_id, result.tx_hash.as_deref());

            assert!(
                result.tx_hash.is_some() || !result.raw.trim().is_empty(),
                "relayer must return a tx_hash or at least a parseable non-empty raw response (neg-risk)"
            );
            tracing::info!(
                target: "vike_polymarket",
                "✅ SUBMITTED NEG-RISK condition_id={} tx_hash={:?} raw={} (submitted, NOT yet settled — see the confirmation line above)", pos.condition_id, result.tx_hash, result.raw
            );
        }
        None => {
            tracing::info!(
                target: "vike_polymarket",
                "no WINNING neg-risk position found on {proxy} — skipping the neg-risk leg"
            );
        }
    }
}
