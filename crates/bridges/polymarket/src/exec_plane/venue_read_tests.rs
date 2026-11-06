use super::*;
use crate::exec_plane::test_support::{FakeServer, Recorded, Reply, declare_direct_egress};

const KEY: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
const STD: &str = "0xE111180000d2663C0091e4f400237545B87B996B";
const NEG: &str = "0xe2222d279d744050d28e00520010520000310F59";
const MAX: &str = "115792089237316195423570985008687907853269984665640564039457584007913129639935";
const TOKEN: &str = "32338220190071351435772801779725302244575775216413325951443816017994629993401";

fn l2() -> PolymarketCreds {
    PolymarketCreds {
        private_key: KEY.to_string(),
        address: "0x00000000000000000000000000000000000000cc".to_string(),
        api_key: "key-1".to_string(),
        secret: "cG9seW1hcmtldC1sMi1zZWNyZXQta2V5LTEyMzQ1Njc4".to_string(),
        passphrase: "pass-1".to_string(),
        relayer_key: String::new(),
        relayer_address: String::new(),
    }
}

// --- the balance -----------------------------------------------------------------------------

/// The venue's per-contract figures become one row each, the two exchanges are named, an approval of
/// `2^256-1` is "unlimited" and not a 3e32 figure, and the amounts are whole pUSD.
#[test]
fn the_balance_names_each_exchange_and_marks_a_forever_approval() {
    let body = serde_json::json!({
        "balance": "12500000",
        "allowances": { STD: "0", NEG: MAX, "0xdead": "2500000" },
    });
    let b = balance_from(&body).unwrap();
    assert_eq!((b.asset.as_str(), b.balance), ("pUSD", 12.5));
    let by = |spender: &str| b.allowances.iter().find(|a| a.spender == spender).unwrap().clone();
    let (std, neg, other) = (by(STD), by(NEG), by("0xdead"));
    assert_eq!((std.name.as_str(), std.amount, std.unlimited), ("CTF exchange", 0.0, false));
    assert_eq!(neg.name, "neg-risk CTF exchange");
    assert!(neg.unlimited, "{neg:?}");
    assert_eq!((other.name.as_str(), other.amount, other.unlimited), ("", 2.5, false));
}

/// The older single-figure shape is kept as ONE row with no spender: the venue did not say which
/// contract it is for, and a row that named one would be a guess.
#[test]
fn the_older_single_allowance_shape_is_one_row_with_no_spender() {
    let b =
        balance_from(&serde_json::json!({ "balance": 7_000_000, "allowance": "3000000" })).unwrap();
    assert_eq!(b.balance, 7.0);
    assert_eq!(b.allowances.len(), 1);
    assert_eq!((b.allowances[0].spender.as_str(), b.allowances[0].amount), ("", 3.0));
}

/// An operator who asked is owed the failure: an answer with no balance, or no object, is an error in
/// words (the pre-order check reads the same bodies as "unknown" and lets the order go).
#[test]
fn a_body_with_no_balance_is_an_error_in_words_not_a_zero() {
    let none = balance_from(&serde_json::json!({ "allowances": {} })).unwrap_err();
    assert!(none.contains("no balance figure"), "{none}");
    let list = balance_from(&serde_json::json!([1, 2])).unwrap_err();
    assert!(list.contains("not an object"), "{list}");
}

// --- the open orders -------------------------------------------------------------------------

fn order_row(id: &str, side: &str, price: &str, size: &str, matched: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "status": "LIVE", "asset_id": TOKEN, "side": side, "price": price,
        "original_size": size, "size_matched": matched, "order_type": "GTC",
        "created_at": 1_700_000_000,
    })
}

/// Every row keeps the limit price it rests at, its side, its fill progress and a status word derived
/// from it; `created_at` seconds become epoch ms; a fresh process names no client order id.
#[test]
fn an_open_order_carries_its_limit_price_side_progress_and_time() {
    let body = serde_json::Value::Array(vec![
        order_row("0xa", "BUY", "0.05", "5", "0"),
        order_row("0xb", "SELL", "0.9", "10", "4"),
    ]);
    let (orders, truncated) = open_orders_from(&body).unwrap();
    assert!(!truncated);
    assert_eq!(orders.len(), 2);
    let a = &orders[0];
    assert_eq!((a.venue_order_id.as_str(), a.symbol.as_str(), a.side), ("0xa", TOKEN, 1));
    assert_eq!((a.price, a.qty, a.filled_qty), (Some(0.05), 5.0, 0.0));
    assert_eq!((a.status.as_str(), a.client_order_id.as_deref()), ("ACCEPTED", None));
    assert_eq!(a.ts_ms, 1_700_000_000_000);
    let b = &orders[1];
    assert_eq!((b.side, b.price, b.filled_qty), (-1, Some(0.9), 4.0));
    assert_eq!(b.status, "PARTIALLY_FILLED");
}

/// An empty list is an ANSWER ("nothing rests"), and a row with no price says so instead of 0.
#[test]
fn an_empty_list_is_an_answer_and_a_missing_price_is_none() {
    let (none, truncated) = open_orders_from(&serde_json::json!([])).unwrap();
    assert!(none.is_empty() && !truncated);
    let mut row = order_row("0xc", "BUY", "0.1", "1", "0");
    row.as_object_mut().unwrap().remove("price");
    let (orders, _) =
        open_orders_from(&serde_json::json!({ "data": [row], "next_cursor": "LTE=" })).unwrap();
    assert_eq!(orders[0].price, None);
    assert!(open_orders_from(&serde_json::json!("nope")).is_err());
}

/// A page the venue says has a successor is returned with `truncated` set (never as if complete); its
/// own end mark is not a successor.
#[test]
fn a_paged_list_is_marked_truncated_and_the_end_mark_is_not_a_page() {
    let row = order_row("0xd", "BUY", "0.1", "1", "0");
    let more = serde_json::json!({ "data": [row.clone()], "next_cursor": "MTAw" });
    assert!(open_orders_from(&more).unwrap().1, "a further page exists");
    let end = serde_json::json!({ "data": [row], "next_cursor": "LTE=" });
    assert!(!open_orders_from(&end).unwrap().1, "LTE= is the end mark");
}

/// More rows than one read carries are cut at the cap and said to be cut.
#[test]
fn rows_past_the_cap_are_cut_and_said_to_be() {
    let rows: Vec<_> = (0..=MAX_VENUE_OPEN_ORDERS)
        .map(|i| order_row(&format!("0x{i}"), "BUY", "0.1", "1", "0"))
        .collect();
    let (orders, truncated) = open_orders_from(&serde_json::Value::Array(rows)).unwrap();
    assert_eq!(orders.len(), MAX_VENUE_OPEN_ORDERS);
    assert!(truncated);
}

// --- the reads against a loopback CLOB -------------------------------------------------------

fn clob(handler: impl Fn(&Recorded) -> Reply + Send + 'static) -> FakeServer {
    declare_direct_egress();
    FakeServer::start(handler)
}

/// ONE signed GET per read, on the endpoint and with the query the venue documents, carrying the L2
/// headers; nothing else is asked.
#[test]
fn each_read_is_one_signed_get_of_its_own_endpoint() {
    let server = clob(|req| match req.path() {
        "/balance-allowance" => Reply::json(
            200,
            serde_json::json!({ "balance": "5000000", "allowances": { STD: MAX } }),
        ),
        "/data/orders" => {
            Reply::json(200, serde_json::json!([order_row("0xe", "BUY", "0.2", "3", "0")]))
        }
        other => Reply::text(404, other),
    });
    let creds = l2();
    let bal =
        read_with(&server.base, &creds, SignatureType::Poly1271, VenueReadKind::Balance).unwrap();
    let VenueRead::Balance(bal) = bal else { panic!("a balance read answers a balance") };
    assert_eq!(bal.balance, 5.0);
    assert!(bal.allowances[0].unlimited);
    let orders =
        read_with(&server.base, &creds, SignatureType::Poly1271, VenueReadKind::OpenOrders)
            .unwrap();
    let VenueRead::OpenOrders { orders, truncated } = orders else { panic!("open orders") };
    assert_eq!((orders.len(), truncated), (1, false));

    let seen = server.seen();
    assert_eq!(seen.len(), 2, "one request per read: {seen:?}");
    assert!(seen.iter().all(|r| r.method == "GET"));
    assert_eq!(seen[0].path(), "/balance-allowance");
    assert_eq!(seen[0].query(), "asset_type=COLLATERAL&signature_type=3");
    assert_eq!(seen[1].path(), "/data/orders");
    assert_eq!(seen[1].query(), format!("limit={MAX_VENUE_OPEN_ORDERS}"));
    for r in &seen {
        assert_eq!(r.header("poly_api_key"), Some("key-1"), "L2-signed: {r:?}");
        assert!(r.header("poly_signature").is_some_and(|s| !s.is_empty()), "{r:?}");
    }
}

/// A refusal carries its status and never the body: an error page from a proxy can echo the request,
/// `POLY_*` headers included, and the sentence goes to an operator's terminal and the node's recent log.
#[test]
fn a_failed_read_says_its_status_and_never_the_response_body() {
    let echo = "Request: POLY_PASSPHRASE: pass-1 POLY_API_KEY: key-1";
    let server = clob(move |_| Reply::text(502, &format!("<html>Bad Gateway {echo}</html>")));
    let why = read_with(&server.base, &l2(), SignatureType::Poly1271, VenueReadKind::Balance)
        .unwrap_err();
    assert!(why.contains("/balance-allowance could not be read"), "{why}");
    assert!(why.contains("bad json (502)"), "the status is kept: {why}");
    for leak in ["pass-1", "key-1", "POLY_PASSPHRASE", "Bad Gateway"] {
        assert!(!why.contains(leak), "the body leaked ({leak}): {why}");
    }
}

/// An account with no private key is refused in words, before any network (this runs against no
/// server at all: a dial would fail differently).
#[test]
fn an_account_with_no_key_is_refused_before_any_network() {
    let why = venue_read(&AccountCredentials::none(), VenueReadKind::OpenOrders).unwrap_err();
    assert!(why.contains("holds no polymarket private key"), "{why}");
    let why = venue_read(&AccountCredentials::none(), VenueReadKind::Positions).unwrap_err();
    assert!(why.contains("holds no polymarket private key"), "{why}");
}

// --- the positions ---------------------------------------------------------------------------

/// The public address of the funded account whose body is committed below (a wallet address is public
/// chain data; no key or credential is in this file).
const FUNDER: &str = "0x107C01D04Fd68557ACd52E89dD01972b22803aD5";

/// A REAL body: the data-api's answer for [`FUNDER`], read from the latency box on 2026-10-31 (HTTP 200, 3732
/// bytes), unedited. Four resolved holdings: three losing legs (price and value 0) and one winner.
const FUNDER_BODY: &str =
    include_str!("../../tests/data/data_api_positions_funder_2026-10-31.json");

fn funder_body() -> serde_json::Value {
    serde_json::from_str(FUNDER_BODY).expect("the committed body is JSON")
}

/// The real wallet's four rows become four positions with the market in words, the side held, the share
/// count and the venue's own price and value; EVERY row says `redeemable` (the market resolved), including
/// the three that pay nothing, which is why the row also carries what a share pays.
#[test]
fn the_real_funder_body_reads_as_four_positions_and_redeemable_is_not_a_winner_flag() {
    let (positions, truncated) = positions_from(&funder_body()).unwrap();
    assert!(!truncated);
    assert_eq!(positions.len(), 4);
    let first = &positions[0];
    assert_eq!(
        first.symbol,
        "1484359444963781501765067677755406951259550728919736807361564486189514136251"
    );
    assert_eq!(first.title, "Solana Up or Down - June 9, 3:40AM-3:45AM ET");
    assert_eq!(first.outcome, "Down");
    assert_eq!((first.qty, first.avg_price), (11.11, Some(0.2099)));
    assert_eq!((first.price, first.value), (Some(0.0), Some(0.0)));
    assert!(positions.iter().all(|p| p.redeemable), "all four markets resolved: {positions:?}");
    let paying: Vec<_> = positions.iter().filter(|p| p.value.is_some_and(|v| v > 0.0)).collect();
    assert_eq!(paying.len(), 1, "only the BNB leg pays: {positions:?}");
    assert_eq!(paying[0].title, "BNB Up or Down - June 12, 4:30AM-4:45AM ET");
    assert_eq!((paying[0].qty, paying[0].price, paying[0].value), (5.0, Some(1.0), Some(5.0)));
}

/// A wallet that holds nothing answers `[]` (measured live), which is an ANSWER and not an error, in
/// either shape the data-api's rows have come in.
#[test]
fn an_empty_wallet_is_an_answer() {
    for body in [serde_json::json!([]), serde_json::json!({ "data": [] })] {
        let (positions, truncated) = positions_from(&body).unwrap();
        assert!(positions.is_empty() && !truncated, "{body}");
    }
}

/// What the venue does not state stays `None` (a missing price is not a price of zero), a quoted number
/// reads as a number, a row naming no token is skipped, and a body that is no list is an error in words.
#[test]
fn a_missing_figure_is_none_a_quoted_one_is_a_number_and_a_nameless_row_is_skipped() {
    let body = serde_json::json!([
        { "asset": "7", "size": "2.5", "avgPrice": "0.4", "title": "T", "outcome": "Yes" },
        { "size": 9, "title": "no token" },
    ]);
    let (positions, _) = positions_from(&body).unwrap();
    assert_eq!(positions.len(), 1, "the nameless row is skipped");
    let p = &positions[0];
    assert_eq!((p.qty, p.avg_price, p.price, p.value), (2.5, Some(0.4), None, None));
    assert!(!p.redeemable, "a row that says nothing is not redeemable");
    let why = positions_from(&serde_json::json!({ "error": "required query param" })).unwrap_err();
    assert!(why.contains("not a list"), "{why}");
}

/// A list of exactly the size asked for may have been cut by the venue's page and is said to be; one
/// short of it is complete.
#[test]
fn a_full_page_is_marked_truncated() {
    let row = |i: usize| serde_json::json!({ "asset": i.to_string(), "size": 1 });
    let page = |n: usize| serde_json::Value::Array((1..=n).map(row).collect());
    let (cut, truncated) = positions_from(&page(MAX_VENUE_POSITIONS)).unwrap();
    assert_eq!(cut.len(), MAX_VENUE_POSITIONS);
    assert!(truncated);
    assert!(!positions_from(&page(MAX_VENUE_POSITIONS - 1)).unwrap().1);
}

/// ONE keyless GET of the data-api's `/positions` for the funder, asking for every holding (a dust
/// holding included) and the most rows one read carries; no L2 header, no derive call, nothing else asked.
#[test]
fn the_read_is_one_unsigned_get_for_the_funder() {
    let server = clob(|_| Reply::text(200, FUNDER_BODY));
    let VenueRead::Positions { positions, truncated } =
        positions_with(&server.base, FUNDER).unwrap()
    else {
        panic!("a positions read answers positions")
    };
    assert_eq!((positions.len(), truncated), (4, false));
    let seen = server.seen();
    assert_eq!(seen.len(), 1, "one request, no derive call: {seen:?}");
    assert_eq!(seen[0].method, "GET");
    assert_eq!(seen[0].path(), "/positions");
    assert_eq!(
        seen[0].query(),
        format!("user={FUNDER}&sizeThreshold=0&limit={MAX_VENUE_POSITIONS}")
    );
    assert!(
        seen[0].headers.iter().all(|(k, _)| !k.starts_with("poly_")),
        "a public read carries no credential header: {:?}",
        seen[0].headers
    );
}

/// An empty wallet through the whole read: `[]` is an empty answer.
#[test]
fn an_empty_wallet_through_the_read_is_an_empty_answer() {
    let server = clob(|_| Reply::text(200, "[]"));
    let read = positions_with(&server.base, FUNDER).unwrap();
    assert_eq!(read, VenueRead::Positions { positions: Vec::new(), truncated: false });
}

/// A refusal carries its status and never the body, and a body that is no JSON is an error in words.
#[test]
fn a_failed_positions_read_says_its_status_and_never_the_body() {
    let server = clob(|_| Reply::text(502, "<html>Bad Gateway secret-from-a-proxy</html>"));
    let why = positions_with(&server.base, FUNDER).unwrap_err();
    assert!(why.contains("/positions could not be read: HTTP 502"), "{why}");
    assert!(!why.contains("secret-from-a-proxy") && !why.contains("Bad Gateway"), "{why}");
    let server = clob(|_| Reply::text(200, "<html>not json</html>"));
    let why = positions_with(&server.base, FUNDER).unwrap_err();
    assert!(why.contains("is not JSON"), "{why}");
    let server =
        clob(|_| Reply::json(400, serde_json::json!({ "error": "required query param 'user'" })));
    let why = positions_with(&server.base, FUNDER).unwrap_err();
    assert!(why.contains("HTTP 400"), "{why}");
}

/// The funder goes into a query string, so only a plain `0x` address is asked for; anything else is
/// refused in words and the venue is not dialled.
#[test]
fn a_funder_that_is_not_an_address_is_refused_before_any_request() {
    let server = clob(|_| Reply::text(200, "[]"));
    for bad in
        ["", "0x12", "0xzzzz01D04Fd68557ACd52E89dD01972b22803aD5", &format!("{FUNDER}&limit=1")]
    {
        let why = positions_with(&server.base, bad).unwrap_err();
        assert!(why.contains("not a 0x address"), "{bad}: {why}");
    }
    assert!(server.seen().is_empty(), "nothing was asked: {:?}", server.seen());
}

/// The signed CLOB seam does not answer a positions read, and says so without dialling anyone.
#[test]
fn the_signed_clob_seam_refuses_positions_without_a_request() {
    let server = clob(|_| Reply::text(200, "[]"));
    let why = read_with(&server.base, &l2(), SignatureType::Poly1271, VenueReadKind::Positions)
        .unwrap_err();
    assert!(why.contains("public data-api"), "{why}");
    assert!(server.seen().is_empty());
}

/// The wallet that holds the shares is the account's named funder, and the key's own EOA only when it
/// names none.
#[test]
fn the_funder_is_the_named_wallet_else_the_keys_own_address() {
    assert_eq!(funder_of(FUNDER.to_string(), "0xeoa".to_string()), FUNDER);
    assert_eq!(funder_of(String::new(), "0xeoa".to_string()), "0xeoa");
}
