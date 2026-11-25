use super::*;
use vike_secrets::account_credentials::CredentialFields;

/// The host a client that sends nothing is handed (the `.invalid` TLD never resolves).
const NO_HOST: &str = "http://unused.invalid";

// A couple of inline sanity checks for the tiny helpers; the exhaustive body-parsing coverage
// (synthetic JSON per endpoint, with the registry re-keying) lives in
// `tests/offline/polymarket_reconcile_parse.rs`, mirroring the ibkr/alpaca recon parse tests.

/// The account holding `fields`, as a start is handed it.
fn holding(fields: &CredentialFields) -> AccountCredentials<'_> {
    AccountCredentials::new(Some(1), Some(fields), None)
}

/// An account holding `pairs` (`(FIELD, value)`).
fn fields(pairs: &[(&str, &str)]) -> CredentialFields {
    CredentialFields::from_pairs(pairs.iter().copied())
}

/// The signature type of an account holding `pairs`.
fn sig_type_of(pairs: &[(&str, &str)]) -> SignatureType {
    signature_type_for_account(&holding(&fields(pairs)))
}

/// **The signature type is per-WALLET, so it is read per-ACCOUNT** — and no arming row can prove
/// it (`crate::adapter`'s `VenueAdapterPolymarket::resolve` consults credentials only), so
/// it is asserted here.
///
/// The field [`signature_type_for_account`] reads IS the key schema's `SIGNATURE_TYPE` word.
#[test]
fn the_signature_type_field_is_the_key_schemas() {
    let schema = vike_venues::credential_schema::schema_for(VENUE).expect("a polymarket row");
    let field = schema.field(SIGNATURE_TYPE).expect("the schema carries a signature type field");
    assert_eq!(field.owner, vike_venues::credential_schema::KeyFieldOwner::Account, "per ACCOUNT");
    assert_eq!(sig_type_of(&[(SIGNATURE_TYPE, "2")]), SignatureType::PolymarketGnosisSafe);
}

/// A labelled account inheriting another account's type signs in a shape the venue rejects — or,
/// on a deposit wallet, names the wrong maker. Each account's type is read out of ITS fields, and
/// the unset case is the type's OWN default (`Polymarket1271`), never another account's value: that is
/// the "no fallback to another account" rule applied to a field that HAS a default.
#[test]
fn the_signature_type_is_read_per_account_and_never_borrowed() {
    let eoa = fields(&[(SIGNATURE_TYPE, "0")]);
    let unset = fields(&[]);
    let mut book = vike_secrets::account_credentials::CredentialRegistry::new();
    book.insert_account(1, SIGNATURE_TYPE, "0");
    assert_eq!(signature_type_for_account(&holding(&eoa)), SignatureType::Eoa, "account 1's own");
    assert_eq!(
        signature_type_for_account(&book.view(VENUE, Some(2))),
        SignatureType::Polymarket1271,
        "an account with no type of its own takes the TYPE's default, not another account's"
    );
    assert_eq!(signature_type_for_account(&holding(&unset)), SignatureType::Polymarket1271);
    assert_eq!(signature_type_for_account(&book.view(VENUE, Some(1))), SignatureType::Eoa);
}

#[test]
fn active_order_status_from_fill_progress() {
    use vike_model::OrderStatus;
    assert_eq!(normalize_order_status("LIVE", 0.0), OrderStatus::Accepted);
    assert_eq!(normalize_order_status("LIVE", 40.0), OrderStatus::PartiallyFilled);
    // B-16: matched to its whole size, the row still rests
    assert_eq!(normalize_order_status("MATCHED", 100.0), OrderStatus::PartiallyFilled);
    assert_eq!(normalize_order_status("CANCELED", 0.0), OrderStatus::Canceled);
}

/// The venue-local opt-in is OFF by default: it is `flags.poly_reconcile` as the root resolved it
/// (`AdapterFlags`), and the default is every flag off.
#[test]
fn reconcile_gate_is_off_by_default() {
    assert!(!polymarket_reconcile_enabled(&AdapterFlags::default()));
    let on = AdapterFlags { poly_reconcile: true, ..AdapterFlags::default() };
    assert!(polymarket_reconcile_enabled(&on));
    let exec_only = AdapterFlags { poly_exec: true, ..AdapterFlags::default() };
    assert!(!polymarket_reconcile_enabled(&exec_only), "another flag is not this gate");
}

/// An account with no `PRIVATE_KEY` field ⇒ no client and NO network call
/// (absent-credentials-is-the-live-gate). This is the CI-safe half of `reconcile_client_for_account`:
/// it must return before the `ensure_l2` round-trip, so this test never touches the network.
#[test]
fn without_a_private_key_the_reconcile_client_is_none_and_offline() {
    let seed = VenueUrls::seed("polymarket");
    assert!(reconcile_client_for_account(&AccountCredentials::none(), &seed).is_none());
    assert!(
        reconcile_client_for_account(&holding(&fields(&[("FUNDER", "0xabc")])), &seed).is_none()
    );
}

/// An unusable `PRIVATE_KEY` fails at the pure EOA derivation — still before any network call,
/// and still `None` rather than a panic or a start failure.
#[test]
fn with_a_bad_private_key_the_reconcile_client_is_none_and_offline() {
    let bad = fields(&[("PRIVATE_KEY", "not-a-key")]);
    assert!(reconcile_client_for_account(&holding(&bad), &VenueUrls::seed("polymarket")).is_none());
}

#[test]
fn signature_type_defaults_to_deposit_wallet() {
    assert_eq!(sig_type_of(&[]), SignatureType::Polymarket1271);
    assert_eq!(sig_type_of(&[(SIGNATURE_TYPE, "3")]).code(), 3);
    assert_eq!(sig_type_of(&[(SIGNATURE_TYPE, "0")]).code(), 0);
    assert_eq!(sig_type_of(&[(SIGNATURE_TYPE, "1")]).code(), 1);
    assert_eq!(sig_type_of(&[(SIGNATURE_TYPE, "2")]).code(), 2);
    // The REAL stored value was written as `3   # POLY_1271 (deposit wallet)`, carrying its
    // comment. Before `first_token` this fell through to the default — which happens to BE 3, so
    // the bug was invisible here but would have silently ignored an explicit `0` (a bare-EOA
    // account) written in the same annotated style.
    assert_eq!(sig_type_of(&[(SIGNATURE_TYPE, "0   # bare EOA")]).code(), 0);
    assert_eq!(sig_type_of(&[(SIGNATURE_TYPE, "3   # POLY_1271 (deposit wallet)")]).code(), 3);
}

// --- the on-chain settlement seam -----------------------------------------------------------

fn settlement(token: &str, qty: f64, price: f64, ts_ms: i64) -> ChainSettlement {
    ChainSettlement {
        condition_id: "0xcond".into(),
        token_id: token.into(),
        qty,
        price,
        payout_usdc: qty * price,
        venue: crate::settlement::chain::RedeemVenue::Ctf,
        tx_hash: format!("0xtx-{token}"),
        block: 1,
        timestamp_ms: ts_ms,
    }
}

/// The dedup contract: a chain settlement's `trade_id` is BYTE-IDENTICAL to the one
/// `resolve::settlement_fill` stamps, so a settlement the resolve poller already folded is
/// deduped by `recon::diff` and raises no divergence.
#[test]
fn settlement_fill_report_shares_the_resolve_trade_id() {
    let s = settlement("tok", 5.0, 1.0, 1_700_000_000_000);
    let f = settlement_fill_report(&s);
    assert_eq!(f.trade_id, crate::settlement::resolve::settlement_trade_id("0xcond", "tok"));
    assert_eq!(f.trade_id, "resolution:0xcond:tok");
    assert_eq!(f.venue, VENUE);
    assert_eq!(f.symbol, "tok");
    assert_eq!(f.side, -1, "a settlement closes the long");
    assert_eq!(f.last_qty.to_bits(), 5.0f64.to_bits());
    assert_eq!(f.last_price.to_bits(), 1.0f64.to_bits());
    assert_eq!(f.commission.to_bits(), 0.0f64.to_bits());
    assert_eq!(f.liquidity_side, LiquiditySide::Unknown);
    assert_eq!(f.order_client_id, None, "no order stands behind a settlement");
    assert_eq!(f.timestamp, 1_700_000_000_000);
}

/// A losing leg settles at 0.0 and is STILL reported — that row is the one that explains a
/// position vanishing with no cash arriving.
#[test]
fn settlement_fill_report_reports_a_zero_payout_loser() {
    let f = settlement_fill_report(&settlement("tokLose", 40.0, 0.0, 1));
    assert_eq!(f.last_price.to_bits(), 0.0f64.to_bits());
    assert_eq!(f.last_qty.to_bits(), 40.0f64.to_bits());
}

/// Without an oracle the chain half contributes NOTHING — the default build's fill fetch is
/// byte-identical to the CLOB-only one.
#[test]
fn chain_fills_are_empty_without_an_oracle() {
    let c = ReconcileClientPolymarket::new(
        PolymarketConfig::default(),
        "0xfunder".into(),
        SignatureType::Polymarket1271,
        OrderVenueIdTracker::new(),
        NO_HOST.into(),
        NO_HOST.into(),
    );
    assert!(c.chain.is_none());
    assert!(c.chain_settlement_fills(0).is_empty());
}

/// With an oracle, observed settlements come back as fills — and the `since` cutoff is honoured
/// (the CLOB half ignores `since`; this half cannot, or every pass would re-report every
/// settlement ever seen).
#[test]
fn chain_fills_report_observed_settlements_and_honour_since() {
    let oracle = Arc::new(ChainOracle::new(crate::settlement::chain::PolygonRpc::with_url(
        "http://127.0.0.1:1/never-dialled",
    )));
    oracle.record_settlements([
        settlement("tokOld", 5.0, 1.0, 1_000),
        settlement("tokNew", 8.0, 0.0, 9_000),
    ]);
    let c = ReconcileClientPolymarket::new(
        PolymarketConfig::default(),
        "0xfunder".into(),
        SignatureType::Polymarket1271,
        OrderVenueIdTracker::new(),
        NO_HOST.into(),
        NO_HOST.into(),
    )
    .with_chain_oracle(oracle);

    let all = c.chain_settlement_fills(0);
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].symbol, "tokOld");
    let recent = c.chain_settlement_fills(5_000);
    assert_eq!(recent.len(), 1, "the older settlement is outside the lookback");
    assert_eq!(recent[0].symbol, "tokNew");
}

#[test]
fn balance_divides_base_units() {
    let v = serde_json::json!({ "balance": "12500000", "allowance": "0" });
    assert_eq!(parse_balance(&v).unwrap(), Some(12.5));
    assert_eq!(parse_balance(&serde_json::json!({})).unwrap(), None);
    assert!(parse_balance(&serde_json::json!([])).is_err());
}

// --- one pass's reads over a loopback double: the route, the walk, a retired path ------------------

/// A REAL `/v2/positions` body of the public sample address, unedited (`tests/data/`).
const V2_SAMPLE: &str =
    include_str!("../../tests/data/data_api_v2_positions_sample_2026-11-07.json");
/// The public sample address the fixture was read for.
const SAMPLE_WALLET: &str = "0x0000000000000000000000000000000000000001";

/// A client whose every host is the double.
fn client_on(base: &str) -> ReconcileClientPolymarket {
    let creds = PolymarketConfig {
        private_key: "0x1111111111111111111111111111111111111111111111111111111111111111"
            .to_string()
            .into(),
        address: "0x00000000000000000000000000000000000000cc".into(),
        api_key: "key-1".to_string().into(),
        secret: "cG9seW1hcmtldC1sMi1zZWNyZXQta2V5LTEyMzQ1Njc4".to_string().into(),
        passphrase: "pass-1".to_string().into(),
        ..Default::default()
    };
    ReconcileClientPolymarket::new(
        creds,
        SAMPLE_WALLET.into(),
        SignatureType::Polymarket1271,
        OrderVenueIdTracker::new(),
        base.to_string(),
        base.to_string(),
    )
}

fn server(
    handler: impl Fn(&crate::execution::test_support::Recorded) -> crate::execution::test_support::Reply
    + Send
    + 'static,
) -> crate::execution::test_support::FakeServer {
    crate::execution::test_support::declare_direct_egress();
    crate::execution::test_support::FakeServer::start(handler)
}

/// What the venue answers on the v1 path since November 2026, measured through Dublin.
fn v1_retired() -> crate::execution::test_support::Reply {
    crate::execution::test_support::Reply::text(
        410,
        r#"{"error":"Data API v1 has been retired","migration":"https://data-api.polymarket.com/v2/"}"#,
    )
}

/// A reconcile pass reads the route the venue still serves, and the real body becomes the holdings it
/// names, with the holding and the entry price the old fields carried. Both of the body's markets have
/// RESOLVED (`redeemable: true`); the first row is served with the flag cleared, so the pass reports
/// that leg and leaves the resolved one out (`position_reports`): the live fetch skips like the parser.
#[test]
fn a_pass_reads_the_v2_positions_route() {
    use crate::execution::test_support::Reply;
    let mut body: serde_json::Value = serde_json::from_str(V2_SAMPLE).expect("the fixture is JSON");
    body["data"][0]["redeemable"] = serde_json::Value::Bool(false);
    let server = server(move |r| {
        if r.path() == "/v2/positions" { Reply::json(200, body.clone()) } else { v1_retired() }
    });
    let reports = client_on(&server.base).fetch_position_status_reports().unwrap();
    assert_eq!(reports.len(), 1, "the resolved leg is no position: {reports:?}");
    assert_eq!((reports[0].qty, reports[0].average_price), (5000.0, 0.0));
    assert!(server.seen().iter().all(|r| r.method == "GET" && r.path() == "/v2/positions"));
}

/// A wallet of more than one page does not read as holding less: an omitted holding would be a false
/// `OrphanLocalPosition` on a real account. Every page is asked with the same anchor and filters.
#[test]
fn a_wallet_of_more_than_one_page_is_read_whole() {
    use crate::execution::test_support::Reply;
    let row = |token: &str, size: f64| {
        serde_json::json!({
            "condition_id": "0xc", "token_id": token, "current_size": size, "avg_price": 0.5
        })
    };
    let (first, second) = (row("1", 2.0), row("2", 3.0));
    let server = server(move |r| {
        if r.path() != "/v2/positions" {
            v1_retired()
        } else if r.query().contains("cursor=") {
            Reply::json(
                200,
                serde_json::json!({ "data": [second.clone()],
                    "pagination": {"limit": 1, "offset": 1, "has_more": false, "next_cursor": null} }),
            )
        } else {
            Reply::json(
                200,
                serde_json::json!({ "data": [first.clone()],
                    "pagination": {"limit": 1, "offset": 0, "has_more": true, "next_cursor": "abc"} }),
            )
        }
    });
    let reports = client_on(&server.base).fetch_position_status_reports().unwrap();
    let held: Vec<_> = reports.iter().map(|p| (p.symbol.as_str(), p.qty)).collect();
    assert_eq!(held, vec![("1", 2.0), ("2", 3.0)]);
}

/// The words a read of the retired `path` hands the reconcile seam: the typed refusal's own text.
fn retired(path: &str) -> String {
    crate::refusals::PolymarketReadRefusal::Retired { path: path.to_string() }.to_string()
}

/// A retired path is a refusal that says so, never an empty book: an empty book would read as a wallet
/// that sold everything.
#[test]
fn a_retired_positions_path_is_a_named_refusal_not_an_empty_book() {
    let server = server(|_| v1_retired());
    let why = client_on(&server.base).fetch_position_status_reports().unwrap_err();
    assert_eq!(why, retired("/v2/positions"));
}

/// The other half of the pass: `run_pass` propagates any fetch error, so a retired CLOB read must say
/// which path the venue took away.
#[test]
fn a_retired_clob_read_names_its_path() {
    use crate::execution::test_support::Reply;
    let server = server(|_| Reply::text(410, r#"{"error":"gone"}"#));
    let client = client_on(&server.base);
    assert_eq!(client.fetch_order_status_reports(0).unwrap_err(), retired("/data/orders"));
    assert_eq!(client.fetch_fill_reports(0).unwrap_err(), retired("/data/trades"));
    assert_eq!(client.fetch_balance().unwrap_err().text, retired("/balance-allowance"));
}

/// A row's text field is a JSON string only: a number, a bool, `null` and an absent key read `""`, and
/// a string is not trimmed. Pinned on the edge inputs before the read moved onto
/// `vike_bridge_core::json`.
#[test]
fn a_text_field_reads_a_json_string_only() {
    let report = |row: Value| {
        let r = parse_order_reports(&serde_json::json!([row]), |_| None).unwrap().remove(0);
        (r.venue_order_id.to_string(), r.symbol, r.side, r.status)
    };
    assert_eq!(
        report(
            serde_json::json!({ "id": "0xab", "asset_id": " 12", "side": "SELL", "status": "LIVE" })
        ),
        ("0xab".to_string(), " 12".to_string(), -1, normalize_order_status("LIVE", 0.0))
    );
    assert_eq!(
        report(serde_json::json!({ "id": 7, "asset_id": true, "side": -1, "status": null })),
        (String::new(), String::new(), 1, normalize_order_status("", 0.0))
    );
}
