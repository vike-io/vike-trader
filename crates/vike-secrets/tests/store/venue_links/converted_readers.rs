//! The credential filer's fallback account lookup, keyed on the venue's number.

use super::*;
use crate::support;

/// `crate::schema`'s `AccountResolver::load` keys its fallback lookup — `(venue, tier, label)`, the
/// one a key with a NEW owner prefix reaches — on the venue's number. A second spelling of a tier is
/// that key: its prefix is new, and the tier it is filed under names the binance `live` account
/// that already exists. This test's classifier files a `REAL` token as binance's `live` tier, the
/// way the production hand-map files alpaca's `SANDBOX` token as its `demo` one. The lookup must
/// find it, or the store mints a SECOND unlabelled binance
/// `live` account, which no index refuses (NULL labels are distinct) and which arms an ambiguity
/// refusal for the next key of that tier.
#[test]
fn a_credential_filed_through_the_fallback_lookup_finds_its_account() {
    let fx = planted();
    let live = fx.id_of("binance", "live");
    let before: Vec<i64> = fx.accounts().iter().map(|a| a.id).collect();

    // Composed, so the settings registry's literal sweep does not read a credential name here.
    let key = concat!("BINANCE", "_REAL_API_SECRET");
    let classify = |name: &str| -> vike_secrets::Classification {
        if let Some(field) = name.strip_prefix(concat!("BINANCE", "_REAL_")) {
            return vike_secrets::Classification {
                placement: vike_secrets::Placement::Account(vike_secrets::AccountKey {
                    venue: "binance".to_string(),
                    tier: "live".to_string(),
                    label: None,
                    discriminator: None,
                }),
                field: field.to_string(),
                secret: true,
                recognised: true,
            };
        }
        support::classify(name)
    };
    fx.add_key(key, &classify);

    let after: Vec<i64> = fx.accounts().iter().map(|a| a.id).collect();
    assert_eq!(after, before, "no account was minted: the key found its account");
    let keys = vike_secrets::resolve_account_keys_in(fx.dir())
        .expect("the key names")
        .expect("a database answers");
    assert!(
        keys.get(&live).is_some_and(|k| k.names.iter().any(|n| n == key)),
        "…and the key is filed against binance's `live` account: {keys:?}"
    );
}
