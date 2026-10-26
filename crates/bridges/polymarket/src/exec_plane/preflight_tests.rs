use super::*;
use crate::exec_plane::order::build_order;
use crate::exec_plane::test_support::{FakeServer, Recorded, Reply, declare_direct_egress};
use std::sync::{Arc, Mutex};
use tracing_test::traced_test;

const KEY: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
const WALLET: &str = "0x00000000000000000000000000000000000000aa";
const STD: &str = "0xE111180000d2663C0091e4f400237545B87B996B";
const NEG: &str = "0xe2222d279d744050d28e00520010520000310F59";
/// A real outcome token id (a live Gamma read, 2026-10-26): 77 digits.
const TOKEN: &str = "32338220190071351435772801779725302244575775216413325951443816017994629993401";
const MAX: &str = "115792089237316195423570985008687907853269984665640564039457584007913129639935";

fn creds(with_relayer: bool) -> PolymarketCreds {
    PolymarketCreds {
        private_key: KEY.to_string(),
        address: "0x00000000000000000000000000000000000000cc".to_string(),
        api_key: "key-1".to_string(),
        secret: "cG9seW1hcmtldC1sMi1zZWNyZXQta2V5LTEyMzQ1Njc4".to_string(),
        passphrase: "pass-1".to_string(),
        relayer_key: if with_relayer {
            "relayer-key-DO-NOT-PRINT".to_string()
        } else {
            String::new()
        },
        relayer_address: if with_relayer {
            "0x00000000000000000000000000000000000000bb".to_string()
        } else {
            String::new()
        },
    }
}

fn order(side: Side, price: f64, size: f64, neg_risk: bool) -> Order {
    build_order(
        price,
        size,
        side,
        TOKEN,
        WALLET,
        WALLET,
        SignatureType::Poly1271,
        1_700_000_000_000,
        7,
        neg_risk,
        [0u8; 32],
    )
}

/// BUY 10 shares at 0.50 → 5 pUSD = 5_000_000 base units.
fn buy(neg_risk: bool) -> Order {
    order(Side::Buy, 0.5, 10.0, neg_risk)
}

fn snap(balance: &str, allowances: &[(&str, &str)]) -> BalanceAllowance {
    let map: serde_json::Map<String, Value> =
        allowances.iter().map(|(k, v)| (k.to_string(), Value::String(v.to_string()))).collect();
    BalanceAllowance::parse(&serde_json::json!({ "balance": balance, "allowances": map }))
        .expect("an object")
}

// --- the parser -----------------------------------------------------------------------------

#[test]
fn both_wire_shapes_parse_and_an_unlimited_allowance_saturates() {
    let per = BalanceAllowance::parse(&serde_json::json!({
        "balance": "12500000",
        "allowances": { STD: "0", NEG: MAX },
    }))
    .unwrap();
    assert_eq!(per.balance, Some(12_500_000));
    assert_eq!(per.allowance_for(STD), Some(0));
    assert_eq!(
        per.allowance_for(&STD.to_ascii_lowercase()),
        Some(0),
        "addresses compare ignoring case"
    );
    assert_eq!(per.allowance_for(NEG), Some(u128::MAX), "2^256-1 does not fit u128: it saturates");
    assert_eq!(
        per.allowance_for("0xdead"),
        None,
        "a contract the venue did not list is UNKNOWN, not zero"
    );

    let old =
        BalanceAllowance::parse(&serde_json::json!({ "balance": 7, "allowance": "3" })).unwrap();
    assert_eq!((old.balance, old.single), (Some(7), Some(3)));
    assert_eq!(old.allowance_for(STD), Some(3), "the single figure answers for any exchange");
}

#[test]
fn what_cannot_be_read_is_unknown_never_zero() {
    for body in [
        serde_json::json!([]),
        serde_json::json!("x"),
        serde_json::json!(null),
        serde_json::json!(5),
    ] {
        assert_eq!(BalanceAllowance::parse(&body), None, "{body}");
    }
    let odd = BalanceAllowance::parse(&serde_json::json!({
        "balance": "12.5", "allowance": -1, "allowances": { STD: null, NEG: "x" },
    }))
    .unwrap();
    assert_eq!(odd.balance, None, "a decimal point is not a base-unit figure");
    assert_eq!(odd.single, None);
    assert!(odd.per_spender.is_empty());
    let empty = BalanceAllowance::parse(&serde_json::json!({})).unwrap();
    assert_eq!(shortfall(&buy(false), &empty), None, "a body that says nothing refuses nothing");
}

// --- the decision ---------------------------------------------------------------------------

/// Every branch of the decision, BUY side: balance first, then the allowance of the order's OWN
/// exchange; a short allowance on the OTHER exchange is not this order's problem.
#[test]
fn a_buy_is_short_on_balance_or_on_the_allowance_of_its_own_exchange() {
    let need = 5_000_000;
    assert_eq!(buy(false).maker_amount, need);
    let ok = snap("5000000", &[(STD, MAX), (NEG, MAX)]);
    assert_eq!(shortfall(&buy(false), &ok), None, "exactly enough passes");

    let poor = snap("4999999", &[(STD, MAX), (NEG, MAX)]);
    assert_eq!(shortfall(&buy(false), &poor), Some(Shortfall::Balance { have: 4_999_999, need }));

    let unapproved = snap("9000000", &[(STD, "0"), (NEG, MAX)]);
    assert_eq!(shortfall(&buy(false), &unapproved), Some(Shortfall::Allowance { have: 0, need }));
    assert_eq!(shortfall(&buy(true), &unapproved), None, "the neg-risk exchange is approved");

    let partial = snap("9000000", &[(STD, "4999999"), (NEG, MAX)]);
    assert_eq!(
        shortfall(&buy(false), &partial),
        Some(Shortfall::Allowance { have: 4_999_999, need })
    );

    // balance is judged before allowance: a poor AND unapproved wallet is told it is poor
    let both = snap("1", &[(STD, "0")]);
    assert!(matches!(shortfall(&buy(false), &both), Some(Shortfall::Balance { .. })));

    // the venue listed no entry for this exchange: unknown, so no allowance refusal
    let unlisted = snap("9000000", &[("0xsomebody-else", "0")]);
    assert_eq!(shortfall(&buy(false), &unlisted), None);
}

#[test]
fn a_sell_needs_the_shares_not_the_collateral() {
    let sell = order(Side::Sell, 0.5, 10.0, false);
    assert_eq!(sell.maker_amount, 10_000_000, "a sell gives its shares");
    assert_eq!(
        shortfall(&sell, &snap("9999999", &[(STD, MAX)])),
        Some(Shortfall::Balance { have: 9_999_999, need: 10_000_000 })
    );
    assert_eq!(shortfall(&sell, &snap("10000000", &[(STD, MAX)])), None);
    assert!(matches!(
        shortfall(&sell, &snap("10000000", &[(STD, "0")])),
        Some(Shortfall::Allowance { .. })
    ));
}

// --- the words ------------------------------------------------------------------------------

/// The refusal names the asset, both amounts, the wallet and — for an allowance — the contract, and
/// says the order was not sent. An operator reading only this line knows what to fix.
#[test]
fn the_words_name_the_asset_the_amounts_the_wallet_and_the_contract() {
    let b = buy(false);
    let w = refusal_words(&b, &Shortfall::Balance { have: 1_250_000, need: 5_000_000 }, "");
    assert!(w.contains("BUY was NOT sent"), "{w}");
    assert!(w.contains("needs 5 pUSD") && w.contains(WALLET) && w.contains("holds 1.25"), "{w}");
    assert!(w.contains("USDC cannot buy until it has been converted"), "{w}");

    let s = order(Side::Sell, 0.5, 10.0, false);
    let w = refusal_words(&s, &Shortfall::Balance { have: 0, need: 10_000_000 }, "");
    assert!(w.contains("SELL was NOT sent") && w.contains("sells 10 shares"), "{w}");
    assert!(w.contains(TOKEN) && w.contains("holds 0 of them"), "{w}");

    let w = refusal_words(&b, &Shortfall::Allowance { have: 0, need: 5_000_000 }, "REPAIR.");
    assert!(w.contains("CTF exchange") && w.contains(STD) && !w.contains("neg-risk"), "{w}");
    assert!(w.contains("spend its pUSD") && w.contains("is 0 and the order needs 5"), "{w}");
    assert!(w.contains("Enable trading") && w.ends_with("REPAIR."), "{w}");

    let n = buy(true);
    let w = refusal_words(&n, &Shortfall::Allowance { have: 1, need: 5_000_000 }, "");
    assert!(w.contains("neg-risk CTF exchange") && w.contains(NEG), "{w}");
    let w = refusal_words(&s, &Shortfall::Allowance { have: 0, need: 10_000_000 }, "");
    assert!(w.contains("SELL was NOT sent") && w.contains("move its outcome tokens"), "{w}");
}

/// A SELL of more than the wallet holds, where it holds SOME, must not say there is nothing to sell:
/// the words state what is true of the figures they print.
#[test]
fn a_sell_the_wallet_can_only_part_cover_does_not_say_there_is_nothing_to_sell() {
    let s = order(Side::Sell, 0.5, 10.0, false);
    let w = refusal_words(&s, &Shortfall::Balance { have: 3_000_000, need: 10_000_000 }, "");
    assert!(w.contains("sells 10 shares") && w.contains("holds 3 of them"), "{w}");
    assert!(!w.contains("nothing to sell"), "the wallet holds 3: {w}");
    assert!(w.contains("less than"), "{w}");
    // a wallet holding none of it is still told so
    let w = refusal_words(&s, &Shortfall::Balance { have: 0, need: 10_000_000 }, "");
    assert!(w.contains("holds 0 of them") && w.contains("nothing to sell"), "{w}");
}

#[test]
fn units_trim_the_six_decimals() {
    assert_eq!(units(0), "0");
    assert_eq!(units(5_000_000), "5");
    assert_eq!(units(12_500_000), "12.5");
    assert_eq!(units(1), "0.000001");
    assert_eq!(units(1_000_001), "1.000001");
}

// --- the read, against a loopback CLOB -------------------------------------------------------

/// A loopback CLOB whose `/balance-allowance` answer a test can change between orders.
fn clob(answer: Arc<Mutex<(u16, Value)>>) -> FakeServer {
    FakeServer::start(move |req: &Recorded| {
        if req.path() == "/balance-allowance" {
            let (status, body) = answer.lock().unwrap().clone();
            Reply::json(status, body)
        } else {
            Reply::json(404, serde_json::json!({ "error": "not found" }))
        }
    })
}

/// A `/balance-allowance` answer: the wallet's balance and the allowance it granted each exchange.
fn answer(balance: &str, to_std: &str, to_neg: &str) -> Arc<Mutex<(u16, Value)>> {
    Arc::new(Mutex::new((
        200,
        serde_json::json!({ "balance": balance, "allowances": { STD: to_std, NEG: to_neg } }),
    )))
}

#[test]
fn a_wallet_with_enough_passes_and_the_read_is_the_signed_collateral_query() {
    declare_direct_egress();
    let server = clob(answer("9000000", MAX, MAX));
    let mut pre = Preflight::new(&server.base, "http://unused.invalid", SignatureType::Poly1271);
    assert_eq!(pre.check(&creds(true), &buy(false)), Ok(()));

    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!((seen[0].method.as_str(), seen[0].path()), ("GET", "/balance-allowance"));
    assert_eq!(seen[0].query(), "asset_type=COLLATERAL&signature_type=3");
    // an L2-signed read: the account's key rides, its private key does not
    assert_eq!(seen[0].header("poly_api_key"), Some("key-1"));
    assert!(
        seen[0].header("poly_signature").is_some() && seen[0].header("poly_timestamp").is_some()
    );
    assert!(!seen[0].headers.iter().any(|(_, v)| v.contains(&KEY[2..])));
}

#[test]
fn a_sell_reads_the_outcome_token_balance() {
    declare_direct_egress();
    let server = clob(answer("0", MAX, MAX));
    let mut pre = Preflight::new(&server.base, "http://unused.invalid", SignatureType::Poly1271);
    let why = pre.check(&creds(true), &order(Side::Sell, 0.5, 10.0, false)).unwrap_err();
    assert!(why.contains("SELL was NOT sent") && why.contains("holds 0 of them"), "{why}");
    assert_eq!(
        server.seen()[0].query(),
        format!("asset_type=CONDITIONAL&token_id={TOKEN}&signature_type=3")
    );
}

#[test]
fn a_poor_wallet_is_refused_before_anything_is_signed_and_a_refusal_is_never_cached() {
    declare_direct_egress();
    let answer = answer("1250000", MAX, MAX);
    let server = clob(answer.clone());
    let mut pre = Preflight::new(&server.base, "http://unused.invalid", SignatureType::Poly1271);
    let why = pre.check(&creds(true), &buy(false)).unwrap_err();
    assert!(why.contains("needs 5 pUSD") && why.contains("holds 1.25"), "{why}");

    // the operator tops the wallet up: the very next order sees it (no stale refusal)
    *answer.lock().unwrap() =
        (200, serde_json::json!({ "balance": "9000000", "allowances": { STD: MAX, NEG: MAX } }));
    assert_eq!(pre.check(&creds(true), &buy(false)), Ok(()));
    assert_eq!(server.seen().len(), 2, "each order re-read: the refusal was not cached");
}

#[test]
fn a_snapshot_with_no_shortfall_serves_a_burst_for_one_read() {
    declare_direct_egress();
    let server = clob(answer("90000000", MAX, MAX));
    let mut pre = Preflight::new(&server.base, "http://unused.invalid", SignatureType::Poly1271);
    for _ in 0..5 {
        assert_eq!(pre.check(&creds(true), &buy(false)), Ok(()));
    }
    assert_eq!(server.seen().len(), 1, "five orders inside the TTL cost one read");

    // a SELL is a different asset: its own read
    assert_eq!(pre.check(&creds(true), &order(Side::Sell, 0.5, 1.0, false)), Ok(()));
    assert_eq!(server.seen().len(), 2);

    // the TTL bounds the staleness
    let mut short_lived =
        Preflight::new(&server.base, "http://unused.invalid", SignatureType::Poly1271)
            .with_ttl(Duration::ZERO);
    assert_eq!(short_lived.check(&creds(true), &buy(false)), Ok(()));
    assert_eq!(short_lived.check(&creds(true), &buy(false)), Ok(()));
    assert_eq!(server.seen().len(), 4, "a zero TTL reads every time");
}

/// Unknown is never a refusal: a venue error, an unreadable body and an unparseable shape all let the
/// order through to the venue, and a failing venue is asked once per TTL, not once per order.
#[test]
fn an_unreadable_wallet_lets_the_order_through_to_the_venue() {
    declare_direct_egress();
    for (status, body) in [
        (500, serde_json::json!({ "error": "boom" })),
        (401, serde_json::json!({ "error": "Unauthorized/Invalid api key" })),
        (200, serde_json::json!([1, 2, 3])),
        (200, serde_json::json!({ "something": "else" })),
    ] {
        let server = clob(Arc::new(Mutex::new((status, body.clone()))));
        let mut pre =
            Preflight::new(&server.base, "http://unused.invalid", SignatureType::Poly1271);
        assert_eq!(pre.check(&creds(true), &buy(false)), Ok(()), "{status} {body}");
        if status >= 400 {
            assert_eq!(pre.check(&creds(true), &buy(false)), Ok(()));
            assert_eq!(server.seen().len(), 1, "a failing read is not retried per order");
        }
    }
    // an unreachable CLOB: nothing listens on this port
    let mut pre = Preflight::new("http://127.0.0.1:1", "http://unused.invalid", SignatureType::Eoa);
    assert_eq!(pre.check(&creds(false), &buy(false)), Ok(()));
}

/// The read runs ON the exec thread in front of an order, so a stalled `/balance-allowance` must not
/// hold the order for the order path's 30 s (about the core's own ack timeout: the order would then
/// go to the venue after the core had already given up on it). Past the read budget the wallet is
/// "unreadable" and the order goes. Red on the base: it waited for the slow answer and refused on it.
#[test]
fn a_stalled_read_is_given_up_on_and_the_order_goes_to_the_venue() {
    declare_direct_egress();
    let server = FakeServer::start(|_| {
        std::thread::sleep(Duration::from_secs(6));
        Reply::json(200, serde_json::json!({ "balance": "0", "allowances": {} }))
    });
    let mut pre = Preflight::new(&server.base, "http://unused.invalid", SignatureType::Poly1271);
    assert_eq!(
        pre.check(&creds(true), &buy(false)),
        Ok(()),
        "an answer that arrives after the read budget refuses nothing"
    );
}

/// A 200 whose body is not an object is as unreadable as a failed request, so it backs off the same
/// way: one read per TTL, not one in front of every order.
#[test]
fn a_body_that_is_not_an_object_is_asked_once_per_ttl() {
    declare_direct_egress();
    let server = clob(Arc::new(Mutex::new((200, serde_json::json!([1, 2, 3])))));
    let mut pre = Preflight::new(&server.base, "http://unused.invalid", SignatureType::Poly1271);
    for _ in 0..3 {
        assert_eq!(pre.check(&creds(true), &buy(false)), Ok(()));
    }
    assert_eq!(server.seen().len(), 1, "three orders inside the TTL cost one read");
}

/// A failed read says so in the log, with its status, and NEVER with the response body: an error
/// page from a proxy or CDN can echo the request, `POLY_*` credential headers included. Both shapes
/// of status error `get_signed` produces are covered: a JSON body (`polymarket 502: <body>`) and a
/// body that is not JSON (`bad json (502): <serde error>: <body>`).
#[traced_test]
#[test]
fn a_failed_read_logs_its_status_and_never_the_response_body() {
    declare_direct_egress();
    let echo = "Request: POLY_PASSPHRASE: pass-1 POLY_API_KEY: key-1";
    for (body, status) in [
        (serde_json::json!({ "error": "Bad Gateway", "echo": echo }).to_string(), "polymarket 502"),
        (format!("<html>Bad Gateway. {echo}</html>"), "bad json (502)"),
    ] {
        let server = FakeServer::start(move |_| Reply::text(502, &body));
        let mut pre =
            Preflight::new(&server.base, "http://unused.invalid", SignatureType::Poly1271);
        assert_eq!(pre.check(&creds(true), &buy(false)), Ok(()));
        assert!(logs_contain("could not read the wallet"), "the failure is still logged");
        assert!(!logs_contain("pass-1") && !logs_contain("key-1"), "the body leaked: {status}");
        assert!(
            !logs_contain("POLY_PASSPHRASE") && !logs_contain("Bad Gateway"),
            "so did its text"
        );
        assert!(logs_contain(status), "the status is what the log keeps: {status}");
    }
}

/// What may be logged of each error shape `get_signed` produces.
#[test]
fn loggable_keeps_a_transport_error_and_cuts_a_status_error_before_its_body() {
    assert_eq!(loggable("network: timed out"), "network: timed out");
    assert_eq!(loggable("polymarket 502: <html>body</html>"), "polymarket 502");
    assert_eq!(
        loggable("bad json (200): expected value at line 1 column 1: nope"),
        "bad json (200)"
    );
    assert_eq!(loggable("no colon at all"), "no colon at all");
    assert!(loggable(&format!("network: {}", "x".repeat(1000))).chars().count() <= 160);
}

/// Only a token id has a balance to read: anything else goes to the venue untouched, with no read.
#[test]
fn a_symbol_that_is_not_a_token_id_is_not_read() {
    declare_direct_egress();
    let server = clob(answer("0", "0", "0"));
    let mut pre = Preflight::new(&server.base, "http://unused.invalid", SignatureType::Poly1271);
    let mut o = buy(false);
    o.token_id = "TOK".to_string();
    assert_eq!(pre.check(&creds(true), &o), Ok(()));
    assert!(server.seen().is_empty());
}

// --- the allowance repair --------------------------------------------------------------------

fn relayer() -> FakeServer {
    FakeServer::start(|req| match (req.method.as_str(), req.path()) {
        ("GET", "/nonce") => Reply::json(200, serde_json::json!({ "nonce": "4" })),
        ("POST", "/submit") => {
            Reply::json(200, serde_json::json!({ "transactionHash": "0xabc123", "state": "NEW" }))
        }
        _ => Reply::text(404, "{}"),
    })
}

/// An unapproved deposit wallet with relayer credentials: THE four approvals go out once, as one
/// batch, the order is still refused (in words, with the transaction), and the next refusal does
/// not send a second batch.
#[test]
fn an_unapproved_deposit_wallet_gets_one_approval_batch_and_a_refusal_naming_it() {
    declare_direct_egress();
    let clob = clob(answer("9000000", "0", "0"));
    let relayer = relayer();
    let mut pre = Preflight::new(&clob.base, &relayer.base, SignatureType::Poly1271);

    let why = pre.check(&creds(true), &buy(false)).unwrap_err();
    assert!(why.contains("has not approved the CTF exchange") && why.contains(STD), "{why}");
    assert!(why.contains("just sent the approvals") && why.contains("0xabc123"), "{why}");
    let sent = relayer.seen();
    assert_eq!(sent.len(), 2, "a nonce read and one submit: {sent:?}");
    let body: Value = serde_json::from_str(&sent[1].body).unwrap();
    assert_eq!(body["depositWalletParams"]["depositWallet"], WALLET);
    assert_eq!(body["depositWalletParams"]["calls"].as_array().map(Vec::len), Some(4));
    assert_eq!(body["nonce"], "4");

    // still unapproved on the next order (the batch is not mined yet): refused again, NOT re-sent
    let why = pre.check(&creds(true), &buy(true)).unwrap_err();
    assert!(why.contains("neg-risk CTF exchange") && why.contains(NEG), "{why}");
    assert!(why.contains("sent the approvals earlier") && why.contains("0xabc123"), "{why}");
    assert_eq!(relayer.seen().len(), 2, "one batch per process");
    // the send cleared the snapshot, and with the batch out the second order asked the CLOB to
    // re-read the chain before refusing: read, refresh, read
    let count = |path: &str| clob.seen().iter().filter(|r| r.path() == path).count();
    assert_eq!(count("/balance-allowance"), 3, "{:?}", clob.seen());
    assert_eq!(count("/balance-allowance/update"), 1);
}

/// The CLOB caches the chain's allowance. Once the approvals are out, a refusal is preceded by a
/// refresh and a second read — and when the refresh makes the cache catch up, the order goes
/// through instead of being refused for an allowance that is already granted.
#[test]
fn after_the_approvals_the_clob_is_asked_to_re_read_the_chain_and_a_caught_up_cache_passes() {
    declare_direct_egress();
    let refreshed = Arc::new(Mutex::new(false));
    let flag = refreshed.clone();
    let clob = FakeServer::start(move |req| match req.path() {
        "/balance-allowance/update" => {
            *flag.lock().unwrap() = true;
            Reply::json(200, serde_json::json!({}))
        }
        "/balance-allowance" => {
            let allowance = if *flag.lock().unwrap() { MAX } else { "0" };
            Reply::json(
                200,
                serde_json::json!({ "balance": "9000000",
                                    "allowances": { STD: allowance, NEG: allowance } }),
            )
        }
        _ => Reply::text(404, "{}"),
    });
    let relayer = relayer();
    let mut pre = Preflight::new(&clob.base, &relayer.base, SignatureType::Poly1271);

    // first order: unapproved, the batch goes out, the order is refused
    assert!(pre.check(&creds(true), &buy(false)).unwrap_err().contains("just sent the approvals"));
    assert!(!*refreshed.lock().unwrap(), "no refresh before there is anything to refresh");
    // second order: the CLOB still says 0 until asked, is asked, and the order now passes
    assert_eq!(pre.check(&creds(true), &buy(false)), Ok(()));
    assert!(*refreshed.lock().unwrap());
    assert_eq!(relayer.seen().len(), 2, "still one batch");
    let update =
        clob.seen().iter().find(|r| r.path() == "/balance-allowance/update").cloned().unwrap();
    assert_eq!(update.query(), "asset_type=COLLATERAL&signature_type=3");
}

/// No approval batch was sent ⇒ nothing to refresh: an unapproved wallet that cannot be repaired is
/// refused without the extra calls.
#[test]
fn no_refresh_is_asked_for_a_wallet_the_bridge_sent_nothing_for() {
    declare_direct_egress();
    let clob = clob(answer("9000000", "0", "0"));
    let mut pre = Preflight::new(&clob.base, "http://unused.invalid", SignatureType::Eoa);
    assert!(pre.check(&creds(true), &buy(false)).is_err());
    assert!(pre.check(&creds(true), &buy(false)).is_err());
    assert!(!clob.seen().iter().any(|r| r.path() == "/balance-allowance/update"));
}

#[test]
fn an_allowance_it_cannot_repair_is_explained_and_sends_nothing() {
    declare_direct_egress();
    let clob = clob(answer("9000000", "0", "0"));
    let relayer = relayer();

    // a deposit wallet whose account has no relayer credentials
    let mut pre = Preflight::new(&clob.base, &relayer.base, SignatureType::Poly1271);
    let why = pre.check(&creds(false), &buy(false)).unwrap_err();
    assert!(why.contains("RELAYER_API_KEY / RELAYER_API_KEY_ADDRESS"), "{why}");
    assert!(why.contains("Enable trading"), "{why}");

    // a bare EOA: its approvals are gas-paying transactions this bridge does not send
    let mut pre = Preflight::new(&clob.base, &relayer.base, SignatureType::Eoa);
    let why = pre.check(&creds(true), &buy(false)).unwrap_err();
    assert!(why.contains("not a deposit wallet") && why.contains("Enable trading"), "{why}");

    assert!(relayer.seen().is_empty(), "nothing was sent for either");
}

#[test]
fn a_failed_approval_batch_is_reported_once_and_not_retried_in_a_loop() {
    declare_direct_egress();
    let clob = clob(answer("9000000", "0", "0"));
    let refusing = FakeServer::start(|req| match req.path() {
        "/nonce" => Reply::json(200, serde_json::json!({ "nonce": "1" })),
        _ => Reply::json(400, serde_json::json!({ "error": "wallet not deployed" })),
    });
    let mut pre = Preflight::new(&clob.base, &refusing.base, SignatureType::Poly1271);
    let why = pre.check(&creds(true), &buy(false)).unwrap_err();
    assert!(
        why.contains("tried to send the approvals") && why.contains("wallet not deployed"),
        "{why}"
    );
    assert!(!why.contains("relayer-key-DO-NOT-PRINT"), "{why}");
    let attempts = refusing.seen().len();
    let why = pre.check(&creds(true), &buy(false)).unwrap_err();
    assert!(why.contains("failed") && why.contains("Enable trading"), "{why}");
    assert_eq!(refusing.seen().len(), attempts, "a failed batch is not re-sent for every order");
}

/// A balance shortfall never triggers an approval: approvals do not put money in a wallet.
#[test]
fn a_poor_wallet_triggers_no_approval_batch() {
    declare_direct_egress();
    let clob = clob(answer("0", "0", "0"));
    let relayer = relayer();
    let mut pre = Preflight::new(&clob.base, &relayer.base, SignatureType::Poly1271);
    let why = pre.check(&creds(true), &buy(false)).unwrap_err();
    assert!(why.contains("holds 0") && !why.contains("approvals"), "{why}");
    assert!(relayer.seen().is_empty());
}
