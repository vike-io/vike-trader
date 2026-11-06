use super::*;
use crate::exec_plane::settlement::redeem_relayer::{
    build_wallet_call_request, build_wallet_calls_request,
};
use crate::exec_plane::test_support::{FakeServer, Reply, declare_direct_egress};
use vike_bridge_core::eip712::eth_address_from_private_key;

// A fixed throwaway test key (NOT a real account): the one `redeem_relayer_tests.rs` signs with.
const KEY: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
const WALLET: &str = "0x00000000000000000000000000000000000000aa";
const STD: &str = "0xE111180000d2663C0091e4f400237545B87B996B";
const NEG: &str = "0xe2222d279d744050d28e00520010520000310F59";

fn creds() -> PolymarketCreds {
    PolymarketCreds {
        private_key: KEY.to_string(),
        relayer_key: "relayer-key-DO-NOT-PRINT".to_string(),
        relayer_address: "0x00000000000000000000000000000000000000bb".to_string(),
        ..Default::default()
    }
}

/// The selectors are derived from the signature literal AND pinned as independent byte literals
/// (the `redeem.rs` / `split_merge.rs` discipline): a typo in either makes the two disagree. The
/// literals are the universally published ERC-20 / ERC-1155 selectors.
#[test]
fn the_selectors_are_derived_and_pinned() {
    assert_eq!(hex::encode(&keccak256(APPROVE_SIG)[0..4]), "095ea7b3");
    assert_eq!(hex::encode(&keccak256(SET_APPROVAL_FOR_ALL_SIG)[0..4]), "a22cb465");
}

#[test]
fn approve_max_is_selector_spender_and_the_unlimited_amount() {
    let data = approve_max_calldata(STD);
    assert_eq!(
        hex::encode(&data),
        format!(
            "095ea7b3{}{}{}",
            "000000000000000000000000",
            "e111180000d2663c0091e4f400237545b87b996b",
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
        ),
        "selector | spender left-padded to 32 bytes | 2^256-1"
    );
    assert_eq!(data.len(), 4 + 64);
}

#[test]
fn set_approval_for_all_is_selector_operator_and_true() {
    let data = set_approval_for_all_calldata(NEG);
    assert_eq!(
        hex::encode(&data),
        format!(
            "a22cb465{}{}{}",
            "000000000000000000000000",
            "e2222d279d744050d28e00520010520000310f59",
            "0000000000000000000000000000000000000000000000000000000000000001"
        )
    );
}

/// Four calls, pUSD first (both exchanges), then the CTF (both exchanges) — and the spenders are
/// exactly the two exchanges an order is signed against, never anything else.
#[test]
fn the_plan_is_four_calls_naming_only_the_two_exchanges() {
    let calls = trading_calls();
    let targets: Vec<&str> = calls.iter().map(|c| c.target).collect();
    assert_eq!(
        targets,
        [Era::V2.collateral(), Era::V2.collateral(), CTF_ADDRESS, CTF_ADDRESS],
        "pUSD then CTF"
    );
    let spenders: Vec<String> =
        calls.iter().map(|c| hex::encode(&c.calldata[4 + 12..4 + 32])).collect();
    assert_eq!(
        spenders,
        [STD, NEG, STD, NEG].map(|a| a.trim_start_matches("0x").to_ascii_lowercase()),
        "std then neg-risk, for the token and for the CTF"
    );
    assert_eq!(exchange_contract(false), STD);
    assert_eq!(exchange_contract(true), NEG);
}

/// One call in the batch builder IS the single-call builder: the redeem path's pinned bytes do not
/// move because a second builder exists.
#[test]
fn a_one_call_batch_is_byte_identical_to_the_single_call_request() {
    let data = approve_max_calldata(STD);
    let single =
        build_wallet_call_request(KEY, WALLET, Era::V2.collateral(), &data, 3, 99).unwrap();
    let batch =
        build_wallet_calls_request(KEY, WALLET, &[(Era::V2.collateral(), &data[..])], 3, 99)
            .unwrap();
    assert_eq!(single.body, batch.body);
    assert_eq!(single.signature, batch.signature);
}

#[test]
fn the_batch_signs_all_four_calls_and_a_changed_call_changes_the_signature() {
    let calls = trading_calls();
    let pairs: Vec<(&str, &[u8])> =
        calls.iter().map(|c| (c.target, c.calldata.as_slice())).collect();
    let req = build_wallet_calls_request(KEY, WALLET, &pairs, 5, 1_000).unwrap();
    let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
    let wire = body["depositWalletParams"]["calls"].as_array().unwrap();
    assert_eq!(wire.len(), 4);
    for (w, c) in wire.iter().zip(&calls) {
        assert_eq!(w["target"], c.target);
        assert_eq!(w["value"], "0");
        assert_eq!(w["data"], format!("0x{}", hex::encode(&c.calldata)));
    }
    assert_eq!(body["nonce"], "5");
    assert_eq!(body["type"], "WALLET");
    assert!(!req.body.contains(&KEY[2..]), "the private key never rides the body");

    // dropping the last call must change what is signed
    let fewer = build_wallet_calls_request(KEY, WALLET, &pairs[..3], 5, 1_000).unwrap();
    assert_ne!(req.signature, fewer.signature);
    // an empty batch and a malformed target are refused, never signed as zero addresses
    assert!(build_wallet_calls_request(KEY, WALLET, &[], 5, 1_000).is_err());
    assert!(build_wallet_calls_request(KEY, WALLET, &[("0xbad", &[1u8][..])], 5, 1_000).is_err());
}

/// The wire, end to end against a loopback relayer: read the nonce for THIS owner with the relayer
/// headers, sign over it, POST one batch of four calls, parse the transaction hash.
#[test]
fn sending_the_approvals_reads_the_nonce_then_posts_one_signed_batch() {
    declare_direct_egress();
    let relayer = FakeServer::start(|req| match (req.method.as_str(), req.path()) {
        ("GET", "/nonce") => Reply::json(200, serde_json::json!({ "nonce": "7" })),
        ("POST", "/submit") => Reply::json(
            200,
            serde_json::json!({ "transactionID": "tx-1", "transactionHash": "0xfeedbeef",
                                "state": "STATE_NEW" }),
        ),
        _ => Reply::text(404, "{}"),
    });
    let sent = send_trading_approvals(&relayer.base, &creds(), WALLET).expect("sent");
    assert_eq!(sent.tx_hash.as_deref(), Some("0xfeedbeef"));

    let seen = relayer.seen();
    assert_eq!(seen.len(), 2, "one nonce read and one submit: {seen:?}");
    let owner = eth_address_from_private_key(KEY).unwrap();
    assert_eq!(seen[0].query(), format!("address={owner}&type=WALLET"));
    let submit = &seen[1];
    for r in &seen {
        assert_eq!(r.header("relayer_api_key"), Some("relayer-key-DO-NOT-PRINT"));
        assert_eq!(r.header("relayer_api_key_address"), Some(creds().relayer_address.as_str()));
    }
    let body: serde_json::Value = serde_json::from_str(&submit.body).unwrap();
    assert_eq!(body["nonce"], "7", "the batch is signed over the nonce the relayer served");
    assert_eq!(
        body["from"].as_str().map(str::to_ascii_lowercase),
        Some(owner.to_ascii_lowercase())
    );
    assert_eq!(body["depositWalletParams"]["depositWallet"], WALLET);
    assert_eq!(body["depositWalletParams"]["calls"].as_array().map(Vec::len), Some(4));
    assert!(!submit.body.contains("relayer-key-DO-NOT-PRINT"), "the relayer key is a header only");
}

/// Every refusal branch: an unset relayer key sends NOTHING; a relayer refusal and a missing nonce
/// surface their cause; none of them carries a secret.
#[test]
fn a_missing_relayer_key_sends_nothing_and_a_relayer_refusal_names_its_status() {
    declare_direct_egress();
    let relayer = FakeServer::start(|req| match req.path() {
        "/nonce" => Reply::json(200, serde_json::json!({ "nonce": 1 })),
        _ => Reply::json(400, serde_json::json!({ "error": "invalid signature" })),
    });

    let mut bare = creds();
    bare.relayer_key.clear();
    let why = send_trading_approvals(&relayer.base, &bare, WALLET).unwrap_err();
    assert!(why.contains("relayer credentials unset"), "{why}");
    assert!(relayer.seen().is_empty(), "no relayer credentials, no request at all");

    let why = send_trading_approvals(&relayer.base, &creds(), WALLET).unwrap_err();
    assert!(why.contains("relayer 400") && why.contains("invalid signature"), "{why}");
    assert!(!why.contains("relayer-key-DO-NOT-PRINT") && !why.contains(&KEY[2..]), "{why}");

    let no_nonce = FakeServer::start(|_| Reply::json(200, serde_json::json!({ "oops": true })));
    let why = send_trading_approvals(&no_nonce.base, &creds(), WALLET).unwrap_err();
    assert!(why.contains("no nonce in relayer response"), "{why}");
    assert_eq!(no_nonce.seen().len(), 1, "no submit after a nonce that did not parse");
}
