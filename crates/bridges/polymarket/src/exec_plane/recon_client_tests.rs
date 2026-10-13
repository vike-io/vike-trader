use super::*;
use vike_secrets::account_credentials::CredentialFields;

// A couple of inline sanity checks for the tiny helpers; the exhaustive body-parsing coverage
// (synthetic JSON per endpoint, with the registry re-keying) lives in
// `tests/offline/polymarket_reconcile_parse.rs`, mirroring the ibkr/alpaca recon parse tests.

/// The account holding `fields`, as a mount is handed it.
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
/// it (`crate::exec_plane::mount`'s `PolymarketVenueMount::resolve` consults credentials only), so
/// it is asserted here.
///
/// The field [`signature_type_for_account`] reads IS the key schema's `SIGNATURE_TYPE` word.
#[test]
fn the_signature_type_field_is_the_key_schemas() {
    let schema = vike_model::credential_schema::schema_for(VENUE).expect("a polymarket row");
    let field = schema.field(SIGNATURE_TYPE).expect("the schema carries a signature type field");
    assert_eq!(field.owner, vike_model::credential_schema::FieldOwner::Account, "per ACCOUNT");
    assert_eq!(sig_type_of(&[(SIGNATURE_TYPE, "2")]), SignatureType::PolyGnosisSafe);
}

/// A labelled account inheriting another account's type signs in a shape the venue rejects — or,
/// on a deposit wallet, names the wrong maker. Each account's type is read out of ITS fields, and
/// the unset case is the type's OWN default (`Poly1271`), never another account's value: that is
/// the "no fallback to another account" rule applied to a field that HAS a default.
#[test]
fn the_signature_type_is_read_per_account_and_never_borrowed() {
    let eoa = fields(&[(SIGNATURE_TYPE, "0")]);
    let unset = fields(&[]);
    let mut book = vike_secrets::account_credentials::CredentialBook::new();
    book.insert_account(1, SIGNATURE_TYPE, "0");
    assert_eq!(signature_type_for_account(&holding(&eoa)), SignatureType::Eoa, "account 1's own");
    assert_eq!(
        signature_type_for_account(&book.view(VENUE, Some(2))),
        SignatureType::Poly1271,
        "an account with no type of its own takes the TYPE's default, not another account's"
    );
    assert_eq!(signature_type_for_account(&holding(&unset)), SignatureType::Poly1271);
    assert_eq!(signature_type_for_account(&book.view(VENUE, Some(1))), SignatureType::Eoa);
}

#[test]
fn active_order_status_from_fill_progress() {
    use vike_model::OrderStatus;
    assert_eq!(normalize_order_status("LIVE", 0.0, 100.0), OrderStatus::Accepted);
    assert_eq!(normalize_order_status("LIVE", 40.0, 100.0), OrderStatus::PartiallyFilled);
    assert_eq!(normalize_order_status("MATCHED", 100.0, 100.0), OrderStatus::Filled);
    assert_eq!(normalize_order_status("CANCELED", 0.0, 100.0), OrderStatus::Canceled);
}

fn vars(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// The venue-local opt-in is OFF by default and accepts the EXACT string `"1"` only (the
/// `VIKE_RECONCILE` idiom), from the deployment's named values.
#[test]
fn reconcile_gate_is_off_by_default_and_exact() {
    assert!(!poly_reconcile_enabled(&vars(&[])));
    assert!(poly_reconcile_enabled(&vars(&[(POLY_RECONCILE_ENV, "1")])));
    // `.env` padding
    assert!(poly_reconcile_enabled(&vars(&[(POLY_RECONCILE_ENV, " 1 ")])));
    // A value is taken verbatim, trailing inline comment included (see `first_token`) — an
    // annotated `1` must still mean on.
    assert!(poly_reconcile_enabled(&vars(&[(POLY_RECONCILE_ENV, "1   # quarantine-first")])));
    for off in ["0", "true", "yes", "on", "", "11", "1x"] {
        assert!(!poly_reconcile_enabled(&vars(&[(POLY_RECONCILE_ENV, off)])), "{off}");
    }
}

/// An account with no `PRIVATE_KEY` field ⇒ no client and NO network call
/// (absent-credentials-is-the-live-gate). This is the CI-safe half of `recon_client_for_account`:
/// it must return before the `ensure_l2` round-trip, so this test never touches the network.
#[test]
fn without_a_private_key_the_recon_client_is_none_and_offline() {
    assert!(recon_client_for_account(&AccountCredentials::none()).is_none());
    assert!(recon_client_for_account(&holding(&fields(&[("FUNDER", "0xabc")]))).is_none());
}

/// An unusable `PRIVATE_KEY` fails at the pure EOA derivation — still before any network call,
/// and still `None` rather than a panic or a mount failure.
#[test]
fn with_a_bad_private_key_the_recon_client_is_none_and_offline() {
    assert!(recon_client_for_account(&holding(&fields(&[("PRIVATE_KEY", "not-a-key")]))).is_none());
}

#[test]
fn signature_type_defaults_to_deposit_wallet() {
    assert_eq!(sig_type_of(&[]), SignatureType::Poly1271);
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
        venue: crate::exec_plane::settlement::chain::RedeemVenue::Ctf,
        tx_hash: format!("0xtx-{token}"),
        block: 1,
        ts_ms,
    }
}

/// The dedup contract: a chain settlement's `trade_id` is BYTE-IDENTICAL to the one
/// `resolve::settlement_fill` stamps, so a settlement the resolve poller already folded is
/// deduped by `recon::diff` and raises no divergence.
#[test]
fn settlement_fill_report_shares_the_resolve_trade_id() {
    let s = settlement("tok", 5.0, 1.0, 1_700_000_000_000);
    let f = settlement_fill_report(&s);
    assert_eq!(
        f.trade_id,
        crate::exec_plane::settlement::resolve::settlement_trade_id("0xcond", "tok")
    );
    assert_eq!(f.trade_id, "resolution:0xcond:tok");
    assert_eq!(f.venue, VENUE);
    assert_eq!(f.symbol, "tok");
    assert_eq!(f.side, -1, "a settlement closes the long");
    assert_eq!(f.last_qty.to_bits(), 5.0f64.to_bits());
    assert_eq!(f.last_px.to_bits(), 1.0f64.to_bits());
    assert_eq!(f.commission.to_bits(), 0.0f64.to_bits());
    assert_eq!(f.liquidity_side, LiquiditySide::Unknown);
    assert_eq!(f.client_order_id, None, "no order stands behind a settlement");
    assert_eq!(f.ts, 1_700_000_000_000);
}

/// A losing leg settles at 0.0 and is STILL reported — that row is the one that explains a
/// position vanishing with no cash arriving.
#[test]
fn settlement_fill_report_reports_a_zero_payout_loser() {
    let f = settlement_fill_report(&settlement("tokLose", 40.0, 0.0, 1));
    assert_eq!(f.last_px.to_bits(), 0.0f64.to_bits());
    assert_eq!(f.last_qty.to_bits(), 40.0f64.to_bits());
}

/// Without an oracle the chain half contributes NOTHING — the default build's fill fetch is
/// byte-identical to the CLOB-only one.
#[test]
fn chain_fills_are_empty_without_an_oracle() {
    let c = PolymarketReconClient::new(
        PolymarketCreds::default(),
        "0xfunder".into(),
        SignatureType::Poly1271,
        PolymarketRegistry::new(),
    );
    assert!(c.chain.is_none());
    assert!(c.chain_settlement_fills(0).is_empty());
}

/// With an oracle, observed settlements come back as fills — and the `since` cutoff is honoured
/// (the CLOB half ignores `since`; this half cannot, or every pass would re-report every
/// settlement ever seen).
#[test]
fn chain_fills_report_observed_settlements_and_honour_since() {
    let oracle =
        Arc::new(ChainOracle::new(crate::exec_plane::settlement::chain::PolygonRpc::with_url(
            "http://127.0.0.1:1/never-dialled",
        )));
    oracle.record_settlements([
        settlement("tokOld", 5.0, 1.0, 1_000),
        settlement("tokNew", 8.0, 0.0, 9_000),
    ]);
    let c = PolymarketReconClient::new(
        PolymarketCreds::default(),
        "0xfunder".into(),
        SignatureType::Poly1271,
        PolymarketRegistry::new(),
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
