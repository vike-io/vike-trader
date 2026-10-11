use super::*;
// Test-only: the sibling contract address the neg-risk fixture must have been emitted by.
use crate::exec_plane::settlement::redeem::NEG_RISK_ADAPTER;

// Every fixture below is a REAL Polygon mainnet log, captured verbatim 2026-07-23 (the block
// numbers and tx hashes are checkable on any explorer). They belong to third-party wallets on
// purpose — this repo's own account never appears in a committed fixture; its verification
// lives in the `#[ignore]`d live smoke, which reads `POLY_FUNDER` from the workspace `.env`.

/// CTF `PayoutRedemption`, tx `0x3f12d5d3…`, payout 59.076955 USDC, indexSets `[1, 2]`.
const CTF_REDEMPTION: &str = r#"{"address":"0x4d97dcd97ec945f40cf65f87097ace5ea0476045","topics":["0x2682012a4a4f1973119f1c9b90745d1bd91fa2bab387344f044cb3586864d18d","0x0000000000000000000000005d4aba8ad45bb5eab3499a0294b42da5d1e455d3","0x0000000000000000000000002791bca1f2de4661ed88a30c99a7a9449aa84174","0x0000000000000000000000000000000000000000000000000000000000000000"],"data":"0x62530e00e2f67d9757e0b06e168e9929e0661daff1276354a3018f1568120c2f0000000000000000000000000000000000000000000000000000000000000060000000000000000000000000000000000000000000000000000000000385715b000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002","blockNumber":"0x5683395","transactionHash":"0x3f12d5d3285700b3b818784725b19c5ac4fcdfc987eff668175aa82aaf41c3cc","blockTimestamp":"0x6a618b45","logIndex":"0x440","removed":false}"#;

/// NegRiskAdapter `PayoutRedemption`, tx `0x92ebcfb0…`, payout 11.0 USDC, amounts `[0, 11.0]`.
const NEG_RISK_REDEMPTION: &str = r#"{"address":"0xd91e80cf2e7be2e162c6513ced06f1dd0da35296","topics":["0x9140a6a270ef945260c03894b3c6b3b2695e9d5101feef0ff24fec960cfd3224","0x00000000000000000000000041792a63ad17e7a210c808de4177e64a561eccee","0x95dbea2403eefccc30a0b4f276e0dd94d8030ac0f826aa2702ebd835cd75985c"],"data":"0x00000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000a7d8c0000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a7d8c0","blockNumber":"0x568339f","transactionHash":"0x92ebcfb0cb9cd1c984d747eeaa6b693103b51f67a822628464a6d9b89b658d0f","blockTimestamp":"0x6a618b54","logIndex":"0x3c2","removed":false}"#;

/// CTF `ConditionResolution`, tx `0x84b7768d…`, 2 slots, numerators `[1, 0]`.
const CONDITION_RESOLUTION: &str = r#"{"address":"0x4d97dcd97ec945f40cf65f87097ace5ea0476045","topics":["0xb44d84d3289691f71497564b85d4233648d9dbae8cbdbb4329f301c3a0185894","0xda4bfca4a2f26cce689ac3e2b89cc60a17fb0b994ee79f61ec8beb65f6886236","0x00000000000000000000000065070be91477460d8a7aeeb94ef92fe056c2f2a7","0xb7576115f85ca1f02c40bd07ae7373640423e87fcffd4c2a8986af23f2a01438"],"data":"0x00000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000040000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000000","blockNumber":"0x56833e9","transactionHash":"0x84b7768df7b4837c89179e776d0575e2c9dfce8f68fa8807de92f423ff40ace7","logIndex":"0x258","removed":false}"#;

/// ERC-1155 `TransferBatch` of TWO outcome tokens, values `[5.0, 0.0]`, tx `0xc6c3f62a…`.
const TRANSFER_BATCH: &str = r#"{"address":"0x4d97dcd97ec945f40cf65f87097ace5ea0476045","topics":["0x4a39dc06d4c0dbc64b70af90fd698a233a518aa5d07e595d983b8c0526c8f7fb","0x000000000000000000000000ada2005600dec949baf300f4c6120000bdb6eaab","0x00000000000000000000000041e7aa1b047f13ad96f26ca49602fb403c90b78d","0x000000000000000000000000ada2005600dec949baf300f4c6120000bdb6eaab"],"data":"0x000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000a0000000000000000000000000000000000000000000000000000000000000000247212f7902c30bd802ea461cc340b9fdb9ad0b35c1ce8aa75807ee264579f66455252c4605d295420c9f788919cbb436e4c17c7690a6dbe43b73bedac1c3069e000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000004c4b400000000000000000000000000000000000000000000000000000000000000000","blockNumber":"0x568339e","transactionHash":"0xc6c3f62a83b1dc2518a78f64878dfe479d4b5ffc98cf0a40f12ae63affdf60e4","removed":false}"#;

/// ERC-1155 `TransferSingle` burn of one outcome token, value 5.0, same tx.
const TRANSFER_SINGLE: &str = r#"{"address":"0x4d97dcd97ec945f40cf65f87097ace5ea0476045","topics":["0xc3d58168c5ae7397731d063d5bbf3d657854427343f4c083240f7aacaa2d0f62","0x000000000000000000000000d91e80cf2e7be2e162c6513ced06f1dd0da35296","0x000000000000000000000000d91e80cf2e7be2e162c6513ced06f1dd0da35296","0x0000000000000000000000000000000000000000000000000000000000000000"],"data":"0x47212f7902c30bd802ea461cc340b9fdb9ad0b35c1ce8aa75807ee264579f66400000000000000000000000000000000000000000000000000000000004c4b40","blockNumber":"0x568339e","transactionHash":"0xc6c3f62a83b1dc2518a78f64878dfe479d4b5ffc98cf0a40f12ae63affdf60e4","removed":false}"#;

fn log(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

// --- word helpers ---------------------------------------------------------------------------

#[test]
fn u256_word_decodes_to_the_decimal_token_id_spelling() {
    // The token id from TRANSFER_SINGLE, in the decimal form `/positions.asset` uses.
    assert_eq!(
        u256_word_to_decimal("47212f7902c30bd802ea461cc340b9fdb9ad0b35c1ce8aa75807ee264579f664")
            .unwrap(),
        "32172845847072308101858152476368078090585804255392742427414441920393186506340"
    );
    assert_eq!(u256_word_to_decimal(&"0".repeat(64)).unwrap(), "0");
    assert_eq!(u256_word_to_decimal(&format!("{:064x}", 1u8)).unwrap(), "1");
    // max uint256 — the schoolbook division must not overflow or truncate.
    assert_eq!(
        u256_word_to_decimal(&"f".repeat(64)).unwrap(),
        "115792089237316195423570985008687907853269984665640564039457584007913129639935"
    );
    assert!(u256_word_to_decimal("dead").is_err(), "a short word is rejected, not padded");
}

#[test]
fn word_u128_refuses_to_truncate_a_wide_value() {
    assert_eq!(word_u128(&format!("{:064x}", 5_000_000u64)).unwrap(), 5_000_000);
    assert!(word_u128(&"f".repeat(64)).is_err(), "a >u128 value must error, never wrap");
    assert!(word_u128("0x00").is_err());
}

#[test]
fn word_address_takes_the_right_aligned_20_bytes() {
    assert_eq!(
        word_address("0x0000000000000000000000005d4aba8ad45bb5eab3499a0294b42da5d1e455d3").unwrap(),
        "0x5d4aba8ad45bb5eab3499a0294b42da5d1e455d3"
    );
}

// --- decoders over REAL mainnet logs --------------------------------------------------------

#[test]
fn decodes_a_real_ctf_redemption() {
    let r = decode_ctf_redemption(&log(CTF_REDEMPTION)).unwrap();
    assert_eq!(r.venue, RedeemVenue::Ctf);
    assert_eq!(r.redeemer, "0x5d4aba8ad45bb5eab3499a0294b42da5d1e455d3");
    assert_eq!(
        r.condition_id,
        "0x62530e00e2f67d9757e0b06e168e9929e0661daff1276354a3018f1568120c2f"
    );
    // 0x0385715b base units = 59.076955 USDC
    assert_eq!(r.payout_usdc.to_bits(), 59.076955f64.to_bits());
    assert_eq!(r.slot_values, vec![1, 2], "the binary redeem's indexSets");
    assert_eq!(r.block, 0x5683395);
    assert_eq!(r.ts_ms, 0x6a618b45 * 1000);
    assert_eq!(r.tx_hash, "0x3f12d5d3285700b3b818784725b19c5ac4fcdfc987eff668175aa82aaf41c3cc");
}

#[test]
fn decodes_a_real_neg_risk_redemption() {
    let r = decode_neg_risk_redemption(&log(NEG_RISK_REDEMPTION)).unwrap();
    assert_eq!(r.venue, RedeemVenue::NegRisk);
    assert_eq!(r.redeemer, "0x41792a63ad17e7a210c808de4177e64a561eccee");
    // The NegRiskAdapter indexes the conditionId as topic2 — proven in the module doc against
    // the CTF's own conditionId word in the same transaction.
    assert_eq!(
        r.condition_id,
        "0x95dbea2403eefccc30a0b4f276e0dd94d8030ac0f826aa2702ebd835cd75985c"
    );
    assert_eq!(r.payout_usdc.to_bits(), 11.0f64.to_bits());
    // per-slot AMOUNTS (the semantics redeem.rs pins from the contract source), not index sets
    assert_eq!(r.slot_values, vec![0, 11_000_000]);
    assert_eq!(r.payout_usdc, r.slot_values[1] as f64 / 1e6, "payout == the winning slot amount");
}

#[test]
fn dispatches_either_redemption_shape_and_rejects_anything_else() {
    assert_eq!(decode_redemption(&log(CTF_REDEMPTION)).unwrap().venue, RedeemVenue::Ctf);
    assert_eq!(decode_redemption(&log(NEG_RISK_REDEMPTION)).unwrap().venue, RedeemVenue::NegRisk);
    assert!(decode_redemption(&log(TRANSFER_BATCH)).is_none());
    assert!(decode_redemption(&log(CONDITION_RESOLUTION)).is_none());
    // The wrong-topic guards are what stop a mis-derived topic0 from being decoded as a
    // plausible-but-wrong redemption.
    assert!(decode_ctf_redemption(&log(NEG_RISK_REDEMPTION)).is_err());
    assert!(decode_neg_risk_redemption(&log(CTF_REDEMPTION)).is_err());
}

#[test]
fn decodes_a_real_condition_resolution() {
    let r = decode_condition_resolution(&log(CONDITION_RESOLUTION)).unwrap();
    assert_eq!(
        r.condition_id,
        "0xda4bfca4a2f26cce689ac3e2b89cc60a17fb0b994ee79f61ec8beb65f6886236"
    );
    assert_eq!(r.oracle, "0x65070be91477460d8a7aeeb94ef92fe056c2f2a7");
    assert_eq!(r.outcome_slot_count, 2);
    assert_eq!(r.payout_numerators, vec![1, 0], "outcome slot 0 won");
    assert!(decode_condition_resolution(&log(CTF_REDEMPTION)).is_err());
}

#[test]
fn decodes_both_erc1155_transfer_shapes_into_one_form() {
    let b = decode_token_transfer(&log(TRANSFER_BATCH)).unwrap();
    assert_eq!(b.from, "0x41e7aa1b047f13ad96f26ca49602fb403c90b78d");
    assert_eq!(b.to, "0xada2005600dec949baf300f4c6120000bdb6eaab");
    assert_eq!(b.ids.len(), 2);
    assert_eq!(b.values, vec![5_000_000, 0]);

    let s = decode_token_transfer(&log(TRANSFER_SINGLE)).unwrap();
    assert_eq!(s.to, "0x0000000000000000000000000000000000000000", "a burn");
    assert_eq!(s.ids.len(), 1);
    assert_eq!(s.values, vec![5_000_000]);
    // The single form's id is the FIRST of the batch's — same transaction, same token.
    assert_eq!(s.ids[0], b.ids[0]);

    assert!(decode_token_transfer(&log(CTF_REDEMPTION)).is_err());
}

// --- ChainResolution semantics --------------------------------------------------------------

#[test]
fn unresolved_condition_prices_nothing() {
    let r = ChainResolution { condition_id: "0xa".into(), denominator: 0, numerators: vec![] };
    assert!(!r.is_resolved());
    assert_eq!(r.payout_for_index(0), None, "unknown must NOT read as worthless");
    assert_eq!(r.winner_index(), None);
}

#[test]
fn binary_resolution_prices_winner_one_and_loser_zero() {
    let r = ChainResolution { condition_id: "0xa".into(), denominator: 1, numerators: vec![0, 1] };
    assert!(r.is_resolved());
    assert_eq!(r.payout_for_index(0).unwrap().to_bits(), 0.0f64.to_bits());
    assert_eq!(r.payout_for_index(1).unwrap().to_bits(), 1.0f64.to_bits());
    assert_eq!(r.winner_index(), Some(1));
    assert_eq!(r.payout_for_index(9), None, "out-of-range slot is unknown, not zero");
}

/// A SPLIT resolution (`[1,1]/2`) pays both legs 0.5 — a payout no `redeemable`-flag heuristic
/// can express, and the reason the numerators are read rather than a boolean.
#[test]
fn split_resolution_prices_both_legs_at_half_and_names_no_winner() {
    let r = ChainResolution { condition_id: "0xa".into(), denominator: 2, numerators: vec![1, 1] };
    assert_eq!(r.payout_for_index(0).unwrap().to_bits(), 0.5f64.to_bits());
    assert_eq!(r.payout_for_index(1).unwrap().to_bits(), 0.5f64.to_bits());
    assert_eq!(r.winner_index(), None, "a split has no single winner");
}

// --- the join ------------------------------------------------------------------------------

fn transfer(ids: &[&str], values: &[u128]) -> TokenTransfer {
    TokenTransfer {
        operator: "0x0".into(),
        from: "0xfunder".into(),
        to: "0x0".into(),
        ids: ids.iter().map(|s| (*s).to_string()).collect(),
        values: values.to_vec(),
        tx_hash: "0xtx".into(),
        block: 1,
    }
}

fn redemption(payout: f64) -> Redemption {
    Redemption {
        venue: RedeemVenue::Ctf,
        redeemer: "0xrelayer".into(),
        condition_id: "0xcond".into(),
        payout_usdc: payout,
        slot_values: vec![1, 2],
        tx_hash: "0xtx".into(),
        block: 1,
        ts_ms: 1_700_000_000_000,
    }
}

#[test]
fn join_prices_each_leg_from_the_chain_resolution() {
    let r =
        ChainResolution { condition_id: "0xcond".into(), denominator: 1, numerators: vec![0, 1] };
    let out = join_settlement(
        &redemption(11.0),
        &[transfer(&["tokYes", "tokNo"], &[4_000_000, 11_000_000])],
        Some(&r),
    );
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].token_id, "tokYes");
    assert_eq!(out[0].price.to_bits(), 0.0f64.to_bits(), "slot 0 lost");
    assert_eq!(out[0].qty.to_bits(), 4.0f64.to_bits());
    assert_eq!(out[1].price.to_bits(), 1.0f64.to_bits(), "slot 1 won");
    assert_eq!(out[1].qty.to_bits(), 11.0f64.to_bits());
    assert_eq!(out[1].payout_usdc.to_bits(), 11.0f64.to_bits());
}

/// Without a chain resolution, a SINGLE non-zero leg is arithmetically forced: it absorbed the
/// whole payout. This is the neg-risk shape seen live (`amounts [0, 11.0]`, payout 11.0).
#[test]
fn join_infers_the_price_when_exactly_one_leg_moved() {
    let out = join_settlement(&redemption(11.0), &[transfer(&["a", "b"], &[0, 11_000_000])], None);
    assert_eq!(out.len(), 1, "zero-amount legs are not settlements");
    assert_eq!(out[0].token_id, "b");
    assert_eq!(out[0].price.to_bits(), 1.0f64.to_bits());
}

/// A loser-only redeem (payout 0) still yields a settlement row — at price 0.0. This is the
/// case `/positions.redeemable` cannot distinguish from "still trading" at all.
#[test]
fn join_settles_a_zero_payout_loser_at_zero() {
    let out = join_settlement(&redemption(0.0), &[transfer(&["a"], &[5_000_000])], None);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].price.to_bits(), 0.0f64.to_bits());
    assert_eq!(out[0].qty.to_bits(), 5.0f64.to_bits());
}

/// Two legs moved and no chain resolution ⇒ the split is under-determined, so NOTHING is
/// synthesised. Refusing to guess is the same rule `resolve::ambiguous_conditions` encodes.
#[test]
fn join_refuses_to_guess_an_underdetermined_split() {
    let out = join_settlement(
        &redemption(11.0),
        &[transfer(&["a", "b"], &[4_000_000, 11_000_000])],
        None,
    );
    assert!(out.is_empty(), "two moved legs + one total is not solvable — emit nothing");
}

/// **The slot-mapping proof.** A single-leg redeem (`indexSets [2]`, observed live) transfers
/// ONE token that is outcome slot **1**, but it sits at vector position 0 — so pricing by
/// position would read the resolution's slot-0 numerator and book a loser at 1.0. The payout
/// identity catches it: `5 shares × 1.0 != 0 USDC`, so rule 1 is rejected and the
/// arithmetically-forced single-leg rule prices it correctly at 0.0.
#[test]
fn join_rejects_a_slot_mapping_that_fails_the_payout_identity() {
    let mut r = redemption(0.0);
    r.slot_values = vec![2]; // a single-leg redeem of slot 1
    // The chain says slot 0 won — so the held slot-1 token is worthless and payout is 0.
    let res =
        ChainResolution { condition_id: "0xcond".into(), denominator: 1, numerators: vec![1, 0] };
    let out = join_settlement(&r, &[transfer(&["tokSlot1"], &[5_000_000])], Some(&res));
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].price.to_bits(),
        0.0f64.to_bits(),
        "position-0 pricing would have said 1.0; the payout identity forced the right answer"
    );
}

/// The identity ACCEPTS a correct mapping — the same shape, but with the winner really at
/// vector position 0 and a matching payout.
#[test]
fn join_accepts_a_slot_mapping_that_satisfies_the_payout_identity() {
    let res =
        ChainResolution { condition_id: "0xcond".into(), denominator: 1, numerators: vec![1, 0] };
    let out =
        join_settlement(&redemption(5.0), &[transfer(&["tokSlot0"], &[5_000_000])], Some(&res));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].price.to_bits(), 1.0f64.to_bits());
    assert_eq!(out[0].payout_usdc.to_bits(), 5.0f64.to_bits());
}

/// A resolution that neither matches the payout NOR leaves a single moved leg is skipped —
/// rule 3, the refuse-to-guess floor.
#[test]
fn join_skips_when_the_identity_fails_and_two_legs_moved() {
    let res =
        ChainResolution { condition_id: "0xcond".into(), denominator: 1, numerators: vec![1, 0] };
    let out = join_settlement(
        &redemption(99.0), // matches neither leg's arithmetic
        &[transfer(&["a", "b"], &[4_000_000, 11_000_000])],
        Some(&res),
    );
    assert!(out.is_empty());
}

#[test]
fn join_of_nothing_is_nothing() {
    assert!(join_settlement(&redemption(1.0), &[], None).is_empty());
    assert!(join_settlement(&redemption(1.0), &[transfer(&[], &[])], None).is_empty());
}

// --- the oracle ------------------------------------------------------------------------------

fn oracle() -> ChainOracle {
    // The URL is never dialled: every test below seeds the oracle instead of fetching.
    ChainOracle::new(PolygonRpc::with_url("http://127.0.0.1:1/never-dialled"))
}

#[test]
fn oracle_caches_a_seeded_resolution_and_normalises_the_key() {
    let o = oracle();
    o.insert_resolution(ChainResolution {
        condition_id: "0xABCD".into(),
        denominator: 1,
        numerators: vec![1, 0],
    });
    // Case- and prefix-insensitive lookup, without a network call.
    assert_eq!(o.resolution("0xabcd").unwrap().numerators, vec![1, 0]);
    assert_eq!(o.resolution("ABCD").unwrap().winner_index(), Some(0));
}

#[test]
fn oracle_dedups_settlements_on_tx_and_token() {
    let o = oracle();
    let s = ChainSettlement {
        condition_id: "0xc".into(),
        token_id: "tok".into(),
        qty: 5.0,
        price: 1.0,
        payout_usdc: 5.0,
        venue: RedeemVenue::Ctf,
        tx_hash: "0xtx".into(),
        block: 1,
        ts_ms: 1_000,
    };
    assert_eq!(o.record_settlements([s.clone()]), 1);
    assert_eq!(o.record_settlements([s.clone()]), 0, "a re-scanned window must not double-book");
    let other = ChainSettlement { token_id: "tok2".into(), ..s };
    assert_eq!(o.record_settlements([other]), 1, "the sibling leg is a distinct row");
    assert_eq!(o.settlements().len(), 2);
    assert_eq!(o.settlements_since(0).len(), 2);
    assert_eq!(o.settlements_since(2_000).len(), 0, "older than the cutoff");
    assert_eq!(o.settlements_since(1_000).len(), 2, "at the cutoff is included");
}

// --- gating ----------------------------------------------------------------------------------

#[test]
fn watcher_rejects_a_malformed_funder() {
    let o = Arc::new(oracle());
    assert!(ChainWatcher::new(Arc::clone(&o), "not-an-address").is_err());
    assert!(ChainWatcher::new(Arc::clone(&o), "0xdead").is_err());
    let w = ChainWatcher::new(o, "0x107C01D04Fd68557ACd52E89dD01972b22803aD5").unwrap();
    assert!(w.cursor().is_none());
    assert_eq!(
        w.funder_topic, "0x000000000000000000000000107c01d04fd68557acd52e89dd01972b22803ad5",
        "the address is left-padded and lower-cased for the topic filter"
    );
}

/// D4: the watcher starts only when its caller says so — no funder, or `enabled == false`, starts
/// nothing. Nothing reads the environment or the store.
#[test]
fn spawn_is_gated_by_its_parameters() {
    let o = Arc::new(oracle());
    let funder = "0x107C01D04Fd68557ACd52E89dD01972b22803aD5".to_string();
    assert!(
        ChainWatchPoller::spawn(Arc::clone(&o), "   ".into(), Duration::from_secs(60), true)
            .is_none()
    );
    assert!(ChainWatchPoller::spawn(o, funder, Duration::from_secs(60), false).is_none());
}

/// The RPC settings' defaults are the documented constants, and a zero window means the default.
#[test]
fn chain_rpc_settings_default_to_the_documented_constants() {
    let s = ChainRpcSettings::default();
    assert_eq!(
        (s.rpc_url.as_str(), s.max_span, s.via_proxy),
        (DEFAULT_RPC_URL, DEFAULT_MAX_SPAN, false)
    );
    let rpc = PolygonRpc::new(&ChainRpcSettings {
        rpc_url: "https://node.example".into(),
        max_span: 7,
        via_proxy: false,
    });
    assert_eq!((rpc.url(), rpc.max_span()), ("https://node.example", 7));
    let zero = PolygonRpc::new(&ChainRpcSettings { max_span: 0, ..ChainRpcSettings::default() });
    assert_eq!(zero.max_span(), DEFAULT_MAX_SPAN, "0 is not a window");
}

/// A paid endpoint's URL can carry its API key, so the settings' `Debug` never prints a URL other
/// than the keyless public default.
#[test]
fn chain_rpc_settings_debug_never_prints_a_custom_url() {
    let keyed = ChainRpcSettings {
        rpc_url: "https://polygon.example/v2/sk-hunter2".into(),
        ..ChainRpcSettings::default()
    };
    let shown = format!("{keyed:?}");
    assert!(!shown.contains("hunter2") && !shown.contains("polygon.example"), "{shown}");
    assert!(format!("{:?}", ChainRpcSettings::default()).contains(DEFAULT_RPC_URL));
}

#[test]
fn rpc_defaults_are_keyless_and_overridable() {
    let r = PolygonRpc::with_url(DEFAULT_RPC_URL);
    assert_eq!(r.url(), DEFAULT_RPC_URL);
    assert_eq!(r.max_span(), DEFAULT_MAX_SPAN);
    // No API key, no query string, no credential of any kind in the default endpoint.
    assert!(!DEFAULT_RPC_URL.contains('?') && !DEFAULT_RPC_URL.contains("key"));
}

/// The pinned constants, asserted as literals so a future edit to a signature string cannot
/// silently change a topic (a wrong topic0 matches NOTHING, which reads as "no redemptions").
#[test]
fn pinned_topics_and_selectors() {
    assert_eq!(
        TOPIC_CONDITION_RESOLUTION,
        "0xb44d84d3289691f71497564b85d4233648d9dbae8cbdbb4329f301c3a0185894"
    );
    assert_eq!(
        TOPIC_CTF_PAYOUT_REDEMPTION,
        "0x2682012a4a4f1973119f1c9b90745d1bd91fa2bab387344f044cb3586864d18d"
    );
    assert_eq!(
        TOPIC_NEG_RISK_PAYOUT_REDEMPTION,
        "0x9140a6a270ef945260c03894b3c6b3b2695e9d5101feef0ff24fec960cfd3224"
    );
    // The two ERC-1155 topics are the universally published constants — they validate the
    // keccak methodology that produced the three Polymarket-specific ones above.
    assert_eq!(
        TOPIC_TRANSFER_SINGLE,
        "0xc3d58168c5ae7397731d063d5bbf3d657854427343f4c083240f7aacaa2d0f62"
    );
    assert_eq!(
        TOPIC_TRANSFER_BATCH,
        "0x4a39dc06d4c0dbc64b70af90fd698a233a518aa5d07e595d983b8c0526c8f7fb"
    );
    assert_eq!(SEL_PAYOUT_DENOMINATOR, "0xdd34de67");
    assert_eq!(SEL_PAYOUT_NUMERATORS, "0x0504c814");
    assert_eq!(SEL_OUTCOME_SLOT_COUNT, "0xd42dc0c2");
    // Each fixture's topic0 IS the pinned constant — the constants and the live wire agree.
    assert_eq!(log(CTF_REDEMPTION)["topics"][0], TOPIC_CTF_PAYOUT_REDEMPTION);
    assert_eq!(log(NEG_RISK_REDEMPTION)["topics"][0], TOPIC_NEG_RISK_PAYOUT_REDEMPTION);
    assert_eq!(log(CONDITION_RESOLUTION)["topics"][0], TOPIC_CONDITION_RESOLUTION);
    assert_eq!(log(TRANSFER_BATCH)["topics"][0], TOPIC_TRANSFER_BATCH);
    assert_eq!(log(TRANSFER_SINGLE)["topics"][0], TOPIC_TRANSFER_SINGLE);
    // And each fixture is emitted by the address this crate already pins for that contract.
    assert_eq!(log(CTF_REDEMPTION)["address"], CTF_ADDRESS.to_ascii_lowercase());
    assert_eq!(log(NEG_RISK_REDEMPTION)["address"], NEG_RISK_ADAPTER.to_ascii_lowercase());
}

// =============================================================================================
// Phase C decoders — fixtures. Every one below is a REAL Polygon mainnet log, sourced from
// the latency box's live ClickHouse tape (`data_polymarket.polymarket_trades` /
// `polymarket_position_events`) by `transaction_hash` + `log_index`, then fetched verbatim via
// `eth_getTransactionReceipt` against the same `polygon.drpc.org` endpoint `PolygonRpc` uses
// (captured 2026-07-25). Expected values were hand-computed from `onchain_decode.py`'s own
// logic (word offsets, side/role assignment) — see the Phase C decoder doc comments for the
// arithmetic — and independently cross-checked against the ClickHouse row the same
// transaction/log_index already decoded to (`polymarket_trades`/`polymarket_position_events`),
// so each fixture is proven against TWO independent decodes, not just this crate's own.
// =============================================================================================

/// V2 `OrderFilled` on [`V2_EXCHANGE`], tx `0x21d27b0f…`, SELL 4.18 @ 0.94 (role: maker). Matches
/// the latency box `polymarket_trades` row for the same tx/log_index exactly (size/price/role/proxy_wallet).
const ORDER_FILL_V2: &str = r#"{"address":"0xe111180000d2663c0091e4f400237545b87b996b","topics":["0xd543adfd945773f1a62f74f0ee55a5e3b9b1a28262980ba90b1a89f2ea84d8ee","0xbfbc54d2a0c2b5b92f572efba77f1c1fa61550d04600fc4f0f1ce420efa08da0","0x000000000000000000000000a253d75c2dfb2c6650291d5e8d54076d2fb40181","0x00000000000000000000000092c9ad93ba0e400ffc8d716edf70a588d246bde3"],"data":"0x0000000000000000000000000000000000000000000000000000000000000001cd997ada6ab1e2cc68c6566e5142cdc885a275d4fa5a1e13b3fdccf22a3f234000000000000000000000000000000000000000000000000000000000003fc82000000000000000000000000000000000000000000000000000000000003bf470000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000","blockNumber":"0x56a1d06","transactionHash":"0x21d27b0fde210ccb5e6480e3f479f342743aa1f854199d57cafab2553d9a516e","transactionIndex":"0x38","blockHash":"0x6fd9b83c01f7d00dec9a7e69fc8427b0b98127e116b02108eeab4cece60ec924","blockTimestamp":"0x6a64696e","logIndex":"0x215","removed":false}"#;

/// V1 (legacy) `OrderFilled` on [`V1_EXCHANGE_STD`], tx `0x31d5dca4…`, BUY 17.7 @ 0.42 (role:
/// maker). Matches the latency box `polymarket_wallet_trades_v2` row for the same tx/log_index exactly.
const ORDER_FILL_V1: &str = r#"{"address":"0x4bfb41d5b3570defd03c39a9a4d8de6bd8b8982e","topics":["0xd0a08e8c493f9c94f29311604c9de1b4e8c8d4c06bd0c789af57f2d65bfec0f6","0xcad66ce0a358cde360f7565f226a7749c1fa08648b57ec3ffd0bf6c0f8a46b8b","0x00000000000000000000000000000003a358014c7c0e227483dbe01619871000","0x000000000000000000000000e55b90febea370d4611a4b0a9ff201183268b24e"],"data":"0x0000000000000000000000000000000000000000000000000000000000000000fdee4e7856e0a259cea17232d8e548f7309655d49c1eac7fe5813a188c3b4b2c0000000000000000000000000000000000000000000000000000000000716f1000000000000000000000000000000000000000000000000000000000010e14a000000000000000000000000000000000000000000000000000000000001b0210","blockNumber":"0x4d93911","transactionHash":"0x31d5dca405136cb4ed09bcf817d2b6c2ce1331b18d4b5738eae2e76f7e73e391","transactionIndex":"0x64","blockHash":"0xc3f40fe4974c608f80d6e9ddca33c61bddc2c6bee0e95f8e6efac8f65a53d26e","blockTimestamp":"0x695e9f3d","logIndex":"0x54d","removed":false}"#;

/// CTF `PositionSplit`, tx `0xc6d1c08c…`, 20 USDC minted. Matches the latency box
/// `polymarket_position_events` (kind=split) row for the same tx/log_index exactly.
const CTF_SPLIT: &str = r#"{"address":"0x4d97dcd97ec945f40cf65f87097ace5ea0476045","topics":["0x2e6bb91f8cbcda0c93623c54d0403a43514fabc40084ec96b6d5379a74786298","0x00000000000000000000000020d2309cd92b797ae7ca175ed828ed8a27fbe29d","0x0000000000000000000000000000000000000000000000000000000000000000","0xb907d819a95244f9e19dc779e3db2a33133a864d22e9ce222d09a16b48759f49"],"data":"0x0000000000000000000000002791bca1f2de4661ed88a30c99a7a9449aa8417400000000000000000000000000000000000000000000000000000000000000600000000000000000000000000000000000000000000000000000000001312d00000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002","blockNumber":"0x52f9360","transactionHash":"0xc6d1c08c7d75a49750e6b282d5f176d79e876419918f78f8ce87ac7141cd9715","transactionIndex":"0x90","blockHash":"0xfe5dcbc7d7e72591cef4b5e98a5d36033f7bdf08ace1ff609d39bf2270e0da1e","blockTimestamp":"0x6a0955e4","logIndex":"0x6f8","removed":false}"#;

/// CTF `PositionsMerge`, tx `0x19ca1abb…`, 21.12 USDC returned. Matches the latency box
/// `polymarket_position_events` (kind=merge) row for the same tx/log_index exactly.
const CTF_MERGE: &str = r#"{"address":"0x4d97dcd97ec945f40cf65f87097ace5ea0476045","topics":["0x6f13ca62553fcc2bcd2372180a43949c1e4cebba603901ede2f4e14f36b282ca","0x000000000000000000000000ada100874d00e3331d00f2007a9c336a65009718","0x0000000000000000000000000000000000000000000000000000000000000000","0x2c1e877e19fe8ebc146e5260c33c9e1eaffd8752f4c3398b49345fbd58fa0e99"],"data":"0x0000000000000000000000002791bca1f2de4661ed88a30c99a7a9449aa8417400000000000000000000000000000000000000000000000000000000000000600000000000000000000000000000000000000000000000000000000001424400000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002","blockNumber":"0x52f9360","transactionHash":"0x19ca1abbc7db11bab75038afe52b915efeac7ba2afac7b610ce2771ff14ea8d1","transactionIndex":"0x8d","blockHash":"0xfe5dcbc7d7e72591cef4b5e98a5d36033f7bdf08ace1ff609d39bf2270e0da1e","blockTimestamp":"0x6a0955e4","logIndex":"0x6db","removed":false}"#;

/// NegRiskAdapter `PositionsConverted`, tx `0xdb22e481…`, 10 USDC, indexSet `0x400` (bit 10).
/// Matches the latency box `polymarket_position_events` (kind=convert) row for the same tx/log_index.
const NEG_RISK_CONVERT: &str = r#"{"address":"0xd91e80cf2e7be2e162c6513ced06f1dd0da35296","topics":["0xb03d19dddbc72a87e735ff0ea3b57bef133ebe44e1894284916a84044deb367e","0x000000000000000000000000ada2005600dec949baf300f4c6120000bdb6eaab","0xc93c202f8849d124e5929d3ef5378d9bd7fe1612b8a30c625c6576b0785adf00","0x0000000000000000000000000000000000000000000000000000000000000400"],"data":"0x0000000000000000000000000000000000000000000000000000000000989680","blockNumber":"0x52f9360","transactionHash":"0xdb22e48179afdd38b97cf157c90f63c9ddcd7be837a951e0d7ad2cc6ec123d2b","transactionIndex":"0x50","blockHash":"0xfe5dcbc7d7e72591cef4b5e98a5d36033f7bdf08ace1ff609d39bf2270e0da1e","blockTimestamp":"0x6a0955e4","logIndex":"0x30c","removed":false}"#;

/// USDC.e `Transfer`, tx `0xe1fa9d91…` — the collateral leg of a CTF redemption (CTF → the
/// redeemer wallet). The SAME transaction's `polymarket_position_events` (kind=redeem) row names
/// this exact wallet as the redeemer, proving this Transfer is that redemption's payout landing.
const USDC_TRANSFER: &str = r#"{"address":"0x2791bca1f2de4661ed88a30c99a7a9449aa84174","topics":["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef","0x0000000000000000000000004d97dcd97ec945f40cf65f87097ace5ea0476045","0x0000000000000000000000006c7e2de5b9d1565793f3757ab568b973413c6816"],"data":"0x00000000000000000000000000000000000000000000000000000000004d1088","blockNumber":"0x52f9360","transactionHash":"0xe1fa9d91dd04fa508bb1d674194b4ece3635ff0b41cf71863d55529667bc48c7","transactionIndex":"0x49","blockHash":"0xfe5dcbc7d7e72591cef4b5e98a5d36033f7bdf08ace1ff609d39bf2270e0da1e","blockTimestamp":"0x6a0955e4","logIndex":"0x288","removed":false}"#;

#[test]
fn phase_c_pinned_topics_and_addresses() {
    assert_eq!(log(ORDER_FILL_V2)["topics"][0], TOPIC_ORDER_FILLED_V2);
    assert_eq!(log(ORDER_FILL_V2)["address"], V2_EXCHANGE);
    assert_eq!(log(ORDER_FILL_V1)["topics"][0], TOPIC_ORDER_FILLED_V1);
    assert_eq!(log(ORDER_FILL_V1)["address"], V1_EXCHANGE_STD);
    assert_eq!(log(CTF_SPLIT)["topics"][0], TOPIC_CTF_POSITION_SPLIT);
    assert_eq!(log(CTF_SPLIT)["address"], CTF_ADDRESS.to_ascii_lowercase());
    assert_eq!(log(CTF_MERGE)["topics"][0], TOPIC_CTF_POSITION_MERGE);
    assert_eq!(log(CTF_MERGE)["address"], CTF_ADDRESS.to_ascii_lowercase());
    assert_eq!(log(NEG_RISK_CONVERT)["topics"][0], TOPIC_NEG_RISK_POSITIONS_CONVERTED);
    assert_eq!(log(NEG_RISK_CONVERT)["address"], NEG_RISK_ADAPTER.to_ascii_lowercase());
    assert_eq!(log(USDC_TRANSFER)["topics"][0], TOPIC_USDC_TRANSFER);
    assert_eq!(
        log(USDC_TRANSFER)["address"],
        crate::exec_plane::settlement::redeem::USDC_E_ADDRESS.to_ascii_lowercase()
    );
}

#[test]
fn decodes_a_real_v2_order_fill() {
    let f = decode_order_fill_v2(&log(ORDER_FILL_V2)).unwrap();
    assert_eq!(f.abi, OrderFillAbi::V2);
    assert_eq!(f.order_hash, "0xbfbc54d2a0c2b5b92f572efba77f1c1fa61550d04600fc4f0f1ce420efa08da0");
    assert_eq!(f.maker, "0xa253d75c2dfb2c6650291d5e8d54076d2fb40181");
    assert_eq!(f.taker, "0x92c9ad93ba0e400ffc8d716edf70a588d246bde3");
    assert_eq!(
        f.token_id,
        "92995309462039665251203590892224533684024782310651466832592596015862972883776"
    );
    assert_eq!(f.side, Side::Sell);
    assert_eq!(f.size.to_bits(), 4.18f64.to_bits());
    // maker_amt=4.18 (SELL: maker gives tokens, gets USDC) / taker_amt=3.9292 -> price=0.94.
    assert_eq!(f.price.to_bits(), 0.9400000000000001f64.to_bits());
    assert_eq!(f.fee_usdc.to_bits(), 0.0f64.to_bits());
    assert_eq!(f.role, FillRole::Maker, "taker topic is a real wallet, not the exchange");
    assert_eq!(f.block, 90_840_326);
    assert_eq!(f.ts_ms, 1_784_965_486_000);
    assert_eq!(f.tx_hash, "0x21d27b0fde210ccb5e6480e3f479f342743aa1f854199d57cafab2553d9a516e");
}

#[test]
fn decodes_a_real_v1_order_fill() {
    let f = decode_order_fill_v1(&log(ORDER_FILL_V1)).unwrap();
    assert_eq!(f.abi, OrderFillAbi::V1);
    assert_eq!(f.order_hash, "0xcad66ce0a358cde360f7565f226a7749c1fa08648b57ec3ffd0bf6c0f8a46b8b");
    assert_eq!(f.maker, "0x00000003a358014c7c0e227483dbe01619871000".to_lowercase());
    assert_eq!(f.taker, "0xe55b90febea370d4611a4b0a9ff201183268b24e");
    assert_eq!(
        f.token_id,
        "114856201873541567677341731370365137173235058495071385195611596664202155739948"
    );
    assert_eq!(f.side, Side::Buy);
    assert_eq!(f.size.to_bits(), 17.7f64.to_bits());
    // makerAssetId==0 -> BUY: usd=makerAmt=7.434 / size=takerAmt=17.7 -> price=0.42000000000000004.
    assert_eq!(f.price.to_bits(), 0.42000000000000004f64.to_bits());
    assert_eq!(f.fee_usdc.to_bits(), 1.77f64.to_bits());
    assert_eq!(f.role, FillRole::Maker, "taker topic is a real wallet, not a V1 exchange");
    assert_eq!(f.block, 81_344_785);
    assert_eq!(f.ts_ms, 1_767_808_829_000);
}

#[test]
fn v1_and_v2_order_fill_decoders_reject_each_others_topic() {
    assert!(decode_order_fill_v2(&log(ORDER_FILL_V1)).is_err());
    assert!(decode_order_fill_v1(&log(ORDER_FILL_V2)).is_err());
    assert!(decode_order_fill_v2(&log(CTF_SPLIT)).is_err());
}

#[test]
fn decodes_a_real_ctf_position_split() {
    let e = decode_ctf_position_split(&log(CTF_SPLIT)).unwrap();
    assert_eq!(e.kind, PositionEventKind::Split);
    assert_eq!(e.stakeholder, "0x20d2309cd92b797ae7ca175ed828ed8a27fbe29d");
    assert_eq!(
        e.condition_id,
        "0xb907d819a95244f9e19dc779e3db2a33133a864d22e9ce222d09a16b48759f49"
    );
    assert_eq!(e.amount.to_bits(), 20.0f64.to_bits());
    assert_eq!(e.block, 87_004_000);
    assert!(decode_ctf_position_split(&log(CTF_MERGE)).is_err(), "wrong topic0 must be rejected");
}

#[test]
fn decodes_a_real_ctf_position_merge() {
    let e = decode_ctf_position_merge(&log(CTF_MERGE)).unwrap();
    assert_eq!(e.kind, PositionEventKind::Merge);
    assert_eq!(e.stakeholder, "0xada100874d00e3331d00f2007a9c336a65009718");
    assert_eq!(
        e.condition_id,
        "0x2c1e877e19fe8ebc146e5260c33c9e1eaffd8752f4c3398b49345fbd58fa0e99"
    );
    assert_eq!(e.amount.to_bits(), 21.12f64.to_bits());
    assert_eq!(e.block, 87_004_000);
    assert!(decode_ctf_position_merge(&log(CTF_SPLIT)).is_err(), "wrong topic0 must be rejected");
}

#[test]
fn decodes_a_real_positions_converted() {
    let c = decode_positions_converted(&log(NEG_RISK_CONVERT)).unwrap();
    assert_eq!(c.stakeholder, "0xada2005600dec949baf300f4c6120000bdb6eaab");
    assert_eq!(c.market_id, "0xc93c202f8849d124e5929d3ef5378d9bd7fe1612b8a30c625c6576b0785adf00");
    // topics[3] = 0x...0400 = 1024 -- a BITMAP over outcome slots, not a per-slot amount.
    assert_eq!(c.index_set, 1024);
    assert_eq!(c.amount.to_bits(), 10.0f64.to_bits());
    assert_eq!(c.block, 87_004_000);
    assert!(decode_positions_converted(&log(CTF_SPLIT)).is_err());
}

#[test]
fn decodes_a_real_usdc_transfer() {
    let t = decode_usdc_transfer(&log(USDC_TRANSFER)).unwrap();
    assert_eq!(t.from, "0x4d97dcd97ec945f40cf65f87097ace5ea0476045", "the CTF contract paying out");
    assert_eq!(t.to, "0x6c7e2de5b9d1565793f3757ab568b973413c6816", "the redeemer wallet");
    // 0x4d1088 base units = 5.050504 USDC.
    assert_eq!(t.value_usdc.to_bits(), 5.050504f64.to_bits());
    assert_eq!(t.block, 87_004_000);
    assert!(decode_usdc_transfer(&log(CTF_SPLIT)).is_err());
}
