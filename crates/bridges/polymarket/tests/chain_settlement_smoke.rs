//! LIVE Polygon read-only smokes for [`vike_polymarket::chain`] — the on-chain settlement watcher.
//!
//! `#[ignore]`d and self-skipping (no credentials ⇒ no test), the same double gate every venue smoke
//! in this crate uses. These are **READ-ONLY**: `eth_blockNumber` / `eth_call` / `eth_getLogs` /
//! `eth_getTransactionReceipt` only. Nothing here signs, and nothing here sends a transaction.
//!
//! Unlike the CLOB smokes these need NO Polymarket credentials and no Dublin tunnel — a public
//! Polygon RPC is not geo-blocked. They only need network. The account-scoped ones additionally
//! need `POLY_FUNDER` in the workspace `.env` (never hard-coded here: this repo's wallet address
//! must not live in a committed file).
//!
//! ```sh
//! cargo test -p vike-polymarket --features polymarket --test chain_settlement_smoke -- --ignored --nocapture
//! ```

#![cfg(feature = "polymarket")]

use std::sync::Arc;

use vike_polymarket::CTF_ADDRESS;
use vike_polymarket::chain::{
    ChainOracle, ChainWatcher, POLYGON_CHAIN_ID, PolygonRpc, TOPIC_CTF_PAYOUT_REDEMPTION,
};

fn workspace_var(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .or_else(|| {
            vike_bridge_core::credentials::load_workspace_dotenv_from(
                std::env::var("VIKE_SETTINGS_DIR").ok().as_deref(),
            )
            .get(key)
            .cloned()
        })
        .map(|v| v.split_whitespace().next().unwrap_or("").to_string())
        .filter(|v| !v.is_empty())
}

/// The RPC actually answers, and it is Polygon — the health check every other smoke rests on.
#[test]
#[ignore]
fn chain_rpc_reaches_polygon() {
    vike_log::test_init();
    let rpc = PolygonRpc::new();
    println!("rpc endpoint: {}", rpc.url());
    let chain_id = rpc.chain_id().expect("eth_chainId");
    assert_eq!(chain_id, POLYGON_CHAIN_ID, "the endpoint must be Polygon mainnet");
    let head = rpc.block_number().expect("eth_blockNumber");
    println!("head block: {head}");
    assert!(head > 60_000_000, "a plausible Polygon head");
}

/// **The topic0 proof.** Scan a recent window for CTF `PayoutRedemption` logs and decode every one.
/// A wrong topic would silently match NOTHING — which is exactly why this asserts a non-empty
/// result rather than merely "did not error": Polymarket redemptions are continuous, so an empty
/// window is itself the failure signal.
#[test]
#[ignore]
fn ctf_redemption_topic_matches_live_logs_and_decodes() {
    vike_log::test_init();
    let rpc = PolygonRpc::new();
    let head = rpc.block_number().expect("head");
    let logs = rpc
        .get_logs(
            CTF_ADDRESS,
            serde_json::json!([TOPIC_CTF_PAYOUT_REDEMPTION]),
            head.saturating_sub(rpc.max_span() - 1),
            head,
        )
        .expect("eth_getLogs");
    println!("{} PayoutRedemption logs in the last {} blocks", logs.len(), rpc.max_span());
    assert!(
        !logs.is_empty(),
        "no redemptions in the window — a wrong topic0 looks exactly like this"
    );
    let mut decoded = 0;
    for l in &logs {
        let r = vike_polymarket::chain::decode_ctf_redemption(l).expect("decode");
        assert!(!r.condition_id.is_empty() && r.condition_id.len() == 66);
        assert!(r.payout_usdc >= 0.0);
        // LIVE FINDING: `indexSets` is NOT always `[1, 2]`. This crate's own
        // `redeem::redeem_positions_calldata` always sends both legs, but other callers redeem a
        // SINGLE leg (`[2]` observed live), so the only invariant is: non-empty, and every element
        // is a valid binary index set.
        assert!(!r.slot_values.is_empty(), "a redeem always names at least one index set");
        assert!(
            r.slot_values.iter().all(|s| *s == 1 || *s == 2),
            "binary index sets are 1 (slot 0) and/or 2 (slot 1), got {:?}",
            r.slot_values
        );
        if decoded < 3 {
            println!(
                "  redeemer {} cond {} payout {} USDC tx {}",
                r.redeemer, r.condition_id, r.payout_usdc, r.tx_hash
            );
        }
        decoded += 1;
    }
    assert_eq!(decoded, logs.len(), "every live log decoded");
}

/// **The `eth_call` payout read**, against this account's OWN conditions. Requires `POLY_FUNDER`
/// plus the Dublin tunnel for the data-api half (positions are geo-blocked; the chain half is not).
///
/// This is the check that refuted `/positions.redeemable` as a winner flag: it prints, per held
/// position, the data-api's `curPrice` beside the chain's `payoutNumerators`, and asserts they
/// AGREE. A disagreement here is a real finding, not a flaky test.
#[test]
#[ignore]
fn account_positions_agree_with_the_chain_payouts() {
    vike_log::test_init();
    let Some(funder) = workspace_var("POLY_FUNDER").or_else(|| workspace_var("POLY_ADDRESS"))
    else {
        eprintln!("skip: no POLY_FUNDER in the workspace .env");
        return;
    };
    let body = match vike_polymarket::get_json(
        vike_polymarket::DATA_API,
        "/positions",
        &format!("user={funder}"),
    ) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("skip: data-api unreachable ({e}) — is the Dublin tunnel up?");
            return;
        }
    };
    // Parsed by the CRATE's own `parse_positions`, never by a second hand-rolled reader: it routes
    // `outcomeIndex` through `parse_outcome_index`, so an absent/garbled/out-of-`u32` slot stays
    // `None` instead of collapsing onto slot 0. A local `as_u64().unwrap_or(0) as u32` is the exact
    // defect #974 closed one layer down — and note `4294967296 as u32 == 0`, so that cast WRAPS
    // onto the PAYING slot of a `[1, 0]` resolution: this loop would then compare slot 0's payout
    // against a different leg's `curPrice` and assert on a pair that was never related.
    let rows = vike_polymarket::parse_positions(&body);
    println!("{} open positions for {funder}", rows.len());
    let rpc = PolygonRpc::new();
    for p in &rows {
        let cond = p.condition_id.as_str();
        let r = rpc.condition_resolution(cond).expect("condition_resolution");
        let chain_px = p.outcome_index.and_then(|i| r.payout_for_index(i));
        println!(
            "  {}\n    cond {cond} idx {:?} redeemable={} curPrice={:?} \
             chain: denom={} numerators={:?} payout={chain_px:?}",
            p.title, p.outcome_index, p.redeemable, p.cur_price, r.denominator, r.numerators,
        );
        // An unreadable slot names no leg to compare, and an absent `curPrice` is not a `0.0`. In
        // either case there is nothing to assert, and asserting anyway is how a degraded page turns
        // into a green run — `Position` keeps both as `Option` precisely so this stays a skip.
        let (Some(chain_px), Some(cur_px)) = (chain_px, p.cur_price) else {
            println!("    (skipped: no readable slot and/or no curPrice to compare against)");
            continue;
        };
        assert!(
            (chain_px - cur_px).abs() < 1e-9,
            "chain payout {chain_px} disagrees with the data-api curPrice {cur_px} for {cond}"
        );
    }
}

/// **The end-to-end watcher**, against this account's own history. Anchors on the funder's ERC-1155
/// outflow (NOT `PayoutRedemption.redeemer`, which is a shared relayer proxy — see the module doc's
/// TRAP section), joins each to its transaction's redemption, and prints the settlements.
///
/// A window with no settlement is a legitimate outcome (this account is small), so the live-tail
/// assertion is on DECODABILITY, not on finding a row. Set `POLY_CHAIN_FROM` (+ optional
/// `POLY_CHAIN_TO`) to replay a KNOWN historical redemption instead — a bounded
/// [`ChainWatcher::scan_range`], which is the end-to-end proof that the funder-outflow join finds a
/// real settlement of this account.
#[test]
#[ignore]
fn watcher_scans_the_account_for_on_chain_settlements() {
    vike_log::test_init();
    let Some(funder) = workspace_var("POLY_FUNDER").or_else(|| workspace_var("POLY_ADDRESS"))
    else {
        eprintln!("skip: no POLY_FUNDER in the workspace .env");
        return;
    };
    let oracle = Arc::new(ChainOracle::from_env());
    let mut watcher = ChainWatcher::new(Arc::clone(&oracle), &funder).expect("watcher");
    let replay = workspace_var("POLY_CHAIN_FROM").and_then(|v| v.parse::<u64>().ok());
    let tick = match replay {
        Some(from) => {
            let to = workspace_var("POLY_CHAIN_TO")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(from + 1);
            let t = watcher.scan_range(from, to).expect("scan_range");
            assert!(
                !t.settlements.is_empty(),
                "an explicit replay window must contain the settlement it was pointed at"
            );
            t
        }
        None => watcher.poll_once().expect("poll_once"),
    };
    println!(
        "scanned blocks {}..={} — {} funder transfers, {} settlements",
        tick.from_block,
        tick.to_block,
        tick.transfers,
        tick.settlements.len()
    );
    for s in &tick.settlements {
        println!(
            "  SETTLEMENT cond {} token {} qty {} @ {} (payout {} USDC, tx {})",
            s.condition_id, s.token_id, s.qty, s.price, s.payout_usdc, s.tx_hash
        );
        assert!(s.qty > 0.0, "a zero-size settlement is never emitted");
        assert!((0.0..=1.0).contains(&s.price), "a CTF payout is collateral per token, in [0, 1]");
    }
    assert_eq!(oracle.settlements().len(), tick.settlements.len(), "recorded into the oracle");
    // Re-scanning the SAME window records nothing new (the (tx, token) dedup).
    if !tick.settlements.is_empty() {
        let again = watcher.scan_range(tick.from_block, tick.to_block).expect("re-scan");
        assert_eq!(
            oracle.settlements().len(),
            tick.settlements.len(),
            "a re-scanned window must not double-book (found {} again)",
            again.settlements.len()
        );
    }
}
