//! The converted readers a revert to the text would otherwise survive.

use super::*;
use crate::support;

// ---------------------------------------------------------------------------------------------
// The converted readers a revert to the text would otherwise survive
// ---------------------------------------------------------------------------------------------

/// `crate::schema`'s `AccountResolver::load` keys its fallback lookup — `(venue, tier, label)`, the
/// one a key with a NEW owner prefix reaches — on the venue's number. A legacy `MAINNET` spelling is
/// that key: its prefix is new, and the tier it normalizes onto names the binance `live` account
/// that already exists. Through a lying text column the lookup must still find it, or the store
/// mints a SECOND unlabelled binance `live` account, which no index refuses (NULL labels are
/// distinct) and which arms an ambiguity refusal for the next key of that tier.
#[test]
fn a_credential_filed_through_the_fallback_lookup_finds_its_account_by_venue_id() {
    let fx = planted();
    let live = fx.id_of("binance", "live");
    let before: Vec<i64> = fx.accounts().iter().map(|a| a.id).collect();
    falsify_the_text_column(&fx);

    // Composed, so the settings registry's literal sweep does not read a credential name here.
    let key = concat!("BINANCE", "_MAINNET_API_SECRET");
    {
        use std::io::Write as _;
        let mut file =
            std::fs::OpenOptions::new().append(true).open(fx.store()).expect("open for append");
        writeln!(file, "{key}={}", support::fake_value(key)).expect("append");
    }
    let classify = |name: &str| -> vike_secrets::Classification {
        if let Some(field) = name.strip_prefix(concat!("BINANCE", "_MAINNET_")) {
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
                pending_move: None,
            };
        }
        support::classify(name)
    };
    vike_secrets::migrate(
        fx.arg(),
        support::is_node_key,
        &classify,
        vike_secrets::WhenNothingToCarry::CreateNothing,
    )
    .expect("file the new key");

    let after: Vec<i64> = fx.accounts().iter().map(|a| a.id).collect();
    assert_eq!(after, before, "no account was minted: the key found its account by venue_id");
    let keys = vike_secrets::resolve_account_keys_in(fx.dir())
        .expect("the key names")
        .expect("a database answers");
    assert!(
        keys.get(&live).is_some_and(|k| k.names.iter().any(|n| n == key)),
        "…and the key is filed against binance's `live` account: {keys:?}"
    );
}

/// `fold_arming_into_accounts` reads BOTH tables by the venue's number — the arming rows it folds
/// FROM and the accounts it folds ONTO. Every bit is cleared first, so only a fold that runs and
/// matches the two tables by number can arm the binance `demo` account under binance's `demo` line.
#[test]
fn the_arming_fold_reads_both_tables_by_venue_id() {
    let fx = planted();
    let demo = fx.id_of("binance", "demo");
    let live = fx.id_of("binance", "live");
    let duka = fx.id_of("dukascopy", "demo");
    falsify_the_text_column(&fx);
    {
        let conn = fx.conn();
        conn.execute("UPDATE account SET armed = 0", []).expect("clear every bit");
    }

    // A write that folds and changes nothing else: the dukascopy account re-stated at its own
    // (absent) label — the account verbs fold on every commit, a no-op one included.
    fx.edit(AccountEdit::Rename { id: duka, label: None }).expect("a no-op rename");

    let armed = |id: i64| fx.row(id).expect("the row").armed;
    assert!(armed(demo), "binance `demo` is armed by binance's `demo` line, read by number");
    assert!(!armed(live), "…and binance `live`, above that line, is not");
}

/// Trap 5's collision check (`crate::schema`'s `carried_key_collisions`) groups by the venue's
/// number: two machine-scoped polymarket rows for one field collide under the shipped
/// `UNIQUE (venue_id, tier, field)` however their text cells read. With the text lying — each row
/// spelled differently — only the number can see it, and the refusal must still name both rows.
#[test]
fn trap_5_finds_a_collision_by_venue_id_when_the_text_column_lies() {
    let (fx, conn) = plant(&support::pre_any_tier_ddl());
    conn.execute_batch(
        "DROP INDEX venue_setting_one_per_machine;
         INSERT INTO venue_setting (id, venue, venue_id, tier, field, value)
             SELECT 1, 'polymarket', id, NULL, 'PROXY_HOST', 'one-value'
             FROM venue WHERE name = 'polymarket';
         INSERT INTO venue_setting (id, venue, venue_id, tier, field, value)
             SELECT 2, 'polymarket', id, NULL, 'PROXY_HOST', 'another-value'
             FROM venue WHERE name = 'polymarket';
         UPDATE venue_setting SET venue = 'text-lies-' || id;",
    )
    .expect("two rows one key would share by number, spelled apart");
    stamp_version(&conn, vike_secrets::SCHEMA_VERSION);
    drop(conn);

    let err = vike_secrets::set_venue_setting_in(fx.dir(), "ibkr", None, "HOST", "<host>")
        .expect_err("step 7's rebuild meets the collision");
    let text = err.to_string();
    assert!(
        text.contains("POLY_PROXY_HOST") && text.contains("rows 1, 2"),
        "the refusal names the key and both rows, found by number: {text}"
    );
    assert!(
        !text.contains("one-value") && !text.contains("another-value"),
        "…and names keys, never values: {text}"
    );
}
