use super::*;
use vike_secrets::account_credentials::CredentialFields;

// The canonical EIP-712 "Cow" key (keccak256("cow")) and its known EOA — reused from the shared
// primitive's spec vectors so we know the derived signer address is correct.
const COW_KEY: &str = "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4";
const COW_ADDRESS: &str = "0xcd2a3d9f938e13cd947ec05abc7fe734df8dd826";

fn signer_with_fixed_nonce() -> SignerAster {
    let creds = Credentials {
        api_key: "0xMASTER".to_string().into(),
        api_secret: COW_KEY.to_string().into(),
        passphrase: None, // force signer derivation from the key
    };
    SignerAster::new(&creds, || 1_700_000_000_000_000)
}

#[test]
fn signer_address_derived_from_key_when_absent() {
    let s = signer_with_fixed_nonce();
    assert_eq!(s.signer_address, COW_ADDRESS);
}

#[test]
fn prepare_is_deterministic_and_carries_auth_fields() {
    let s = signer_with_fixed_nonce();
    let params = [("symbol", "BTCUSDT".to_string()), ("side", "BUY".to_string())];
    let a = s.prepare(&params, "POST", "/fapi/v3/order");
    let b = s.prepare(&params, "POST", "/fapi/v3/order");
    assert_eq!(a.query, b.query, "fixed nonce ⇒ deterministic");
    assert!(a.query.contains("user=0xMASTER"));
    assert!(a.query.contains(&format!("signer={COW_ADDRESS}")));
    assert!(a.query.contains("nonce=1700000000000000"));
    assert!(a.query.contains("signature=0x"));
    assert!(a.headers.is_empty(), "v3 auth is in the query, not headers");
}

/// **At zero skew the signed bytes are identical**: a clock nobody read, and one read as zero, sign what a
/// signer on the bare local clock signs. An offset moves the nonce by its time, in µs, and nothing else.
#[test]
fn the_venue_clock_moves_the_nonce_by_its_offset_and_no_byte_at_zero() {
    use vike_bridge_core::signer::VenueClock;

    const LOCAL_US: i64 = 1_700_000_000_000_000;
    let creds = Credentials {
        api_key: "0xMASTER".to_string().into(),
        api_secret: COW_KEY.to_string().into(),
        passphrase: None,
    };
    let params = [("symbol", "BTCUSDT".to_string())];
    let signed = |signer: SignerAster| signer.prepare(&params, "POST", "/fapi/v3/order").query;
    let bare = signed(SignerAster::new(&creds, || LOCAL_US));

    let read_as_zero = VenueClock::default();
    read_as_zero.read_if_due(0, |_| Ok::<_, String>(0)).unwrap();
    for clock in [VenueClock::default(), read_as_zero] {
        assert_eq!(signed(SignerAster::new(&creds, clock.us_from(|| LOCAL_US))), bare);
    }

    let ahead = VenueClock::default();
    ahead.read_if_due(0, |_| Ok::<_, String>(7)).unwrap();
    let moved = signed(SignerAster::new(&creds, ahead.us_from(|| LOCAL_US)));
    assert!(moved.contains("nonce=1700000000007000"), "{moved}");
    assert_eq!(moved, signed(SignerAster::new(&creds, || LOCAL_US + 7_000)));
}

#[test]
fn nonce_binds_the_signature() {
    let creds = Credentials {
        api_key: "0xMASTER".to_string().into(),
        api_secret: COW_KEY.to_string().into(),
        passphrase: Some(COW_ADDRESS.to_string().into()),
    };
    let s1 = SignerAster::new(&creds, || 1_700_000_000_000_000);
    let s2 = SignerAster::new(&creds, || 1_700_000_000_000_001);
    let p = [("symbol", "BTCUSDT".to_string())];
    assert_ne!(s1.prepare(&p, "POST", "/x").query, s2.prepare(&p, "POST", "/x").query);
}

/// The loader reads the ACCOUNT's fields: nothing is the live gate, a half-written pair (or a blank
/// field) is absent too, and the complete pair maps onto the generic slots with `SIGNER` riding
/// the passphrase slot only when present.
#[test]
fn loader_gate_reads_the_accounts_fields() {
    let none = CredentialFields::new();
    assert!(load_aster_credentials(&AccountCredentials::new(Some(1), Some(&none), None)).is_none());
    assert!(load_aster_credentials(&AccountCredentials::none()).is_none()); // absent ⇒ gate
    for half in [
        CredentialFields::from_pairs([(USER, "0xUser")]),
        CredentialFields::from_pairs([(PRIVATE_KEY, COW_KEY)]),
        CredentialFields::from_pairs([(USER, "  "), (PRIVATE_KEY, COW_KEY)]),
    ] {
        let creds = AccountCredentials::new(Some(1), Some(&half), None);
        assert!(load_aster_credentials(&creds).is_none(), "{half:?} is not a usable pair");
    }
    let pair = CredentialFields::from_pairs([(USER, "0xUser"), (PRIVATE_KEY, COW_KEY)]);
    let c = load_aster_credentials(&AccountCredentials::new(Some(1), Some(&pair), None))
        .expect("the account's pair");
    assert_eq!(c.api_key.expose(), "0xUser");
    assert_eq!(c.api_secret.expose(), COW_KEY);
    assert!(c.passphrase.is_none(), "no SIGNER field ⇒ the signer address is derived from the key");
    let with_signer = CredentialFields::from_pairs([
        (USER, "0xUser"),
        (PRIVATE_KEY, COW_KEY),
        (SIGNER, COW_ADDRESS),
    ]);
    let c = load_aster_credentials(&AccountCredentials::new(Some(1), Some(&with_signer), None))
        .expect("the account's pair");
    assert_eq!(c.passphrase.as_ref().map(|p| p.expose().as_str()), Some(COW_ADDRESS));
}

#[test]
fn debug_redacts_key() {
    let dbg = format!("{:?}", signer_with_fixed_nonce());
    assert!(!dbg.contains(COW_KEY), "private key must never appear in Debug");
    assert!(dbg.contains("key=***"));
}

/// **The fields the loader reads are `vike_venues::credential_schema`'s aster row, word for word**:
/// [`USER`], [`PRIVATE_KEY`] and [`SIGNER`] are exactly the row's ACCOUNT fields, the row's
/// REQUIRED ones are the two the loader refuses without, and an account holding exactly those
/// configures it.
#[test]
fn the_fields_are_the_schemas() {
    use std::collections::BTreeSet;
    use vike_venues::credential_schema::{KeyFieldOwner, schema_for};
    let schema = schema_for(crate::transport::urls::VENUE).expect("aster is a schema row");
    let ours: BTreeSet<&str> = [USER, PRIVATE_KEY, SIGNER].into_iter().collect();
    let theirs: BTreeSet<&str> = schema.account_fields().map(|f| f.field).collect();
    assert_eq!(ours, theirs, "the loader's fields are the schema row's account fields");
    assert!(schema.venue_fields().next().is_none(), "aster holds no venue application field");
    let required = schema.required(KeyFieldOwner::Account);
    assert_eq!(required, [USER, PRIVATE_KEY], "the loader refuses without exactly these");
    let only_required = CredentialFields::from_pairs(required.iter().map(|f| (*f, COW_KEY)));
    assert!(
        load_aster_credentials(&AccountCredentials::new(Some(1), Some(&only_required), None))
            .is_some(),
        "the schema's required fields must configure the loader"
    );
}
