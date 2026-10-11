//! Every venue link is a number: the shipped shape carries no text venue, the venue-scoped
//! credential names its venue by number, and the scope check holds.

use super::*;

/// Every venue-scoped credential row, alias rows included, carries the venue's number.
///
/// ⚠ `populated` keeps it from passing on an empty question: the shared fixture files no
/// venue-scoped credential, so [`planted`] adds one, and this asserts the row is there.
///
/// ⚠ **The number is ALL such a row carries**: `credential` has no text venue. What is left to
/// ask is that the row [`planted`] filed for `ctrader` names `ctrader` by its number — the only
/// place that venue is written down — and that no other row of the table names a venue at all
/// (every other credential the fixture files is account-scoped).
#[test]
fn every_venue_scoped_credential_row_names_its_venue_by_number() {
    let fx = planted();
    let conn = fx.conn();
    let mut stmt = conn
        .prepare(
            "SELECT c.name, v.name FROM credential c JOIN venue v ON v.id = c.venue_id \
             ORDER BY c.name",
        )
        .expect("prepare");
    let named: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        named,
        [("CTRADER_CLIENT_ID".to_string(), "ctrader".to_string())],
        "the fixture's one venue-scoped credential names its venue by number, and nothing else \
         names one"
    );
}

/// **NO table of the shipped shape carries a text venue column, and every reader still answers** —
/// asked of a store born on the shipped batch, over every table the engine holds, through the
/// readers. The last text link, `venue_arming.venue`, left the batch with that table (decision
/// 0119), so a venue can be read only by its number.
#[test]
fn the_shipped_shape_carries_no_text_venue_and_readers_still_answer() {
    let fx = planted();
    let conn = fx.conn();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .map(Result::unwrap)
        .collect();
    // The anti-vacuity guard: the linked tables are among the ones asked about.
    for linked in ["account", "credential", "venue_setting"] {
        assert!(tables.iter().any(|t| t == linked), "`{linked}` is in the shipped shape");
    }
    for table in &tables {
        assert!(!has_text_venue(&conn, table), "`{table}` must carry no text venue column");
    }
    let mut venues: Vec<String> = fx.accounts().into_iter().map(|a| a.venue).collect();
    venues.sort();
    assert_eq!(venues, ["binance", "binance", "dukascopy"]);
    let rows = settings_rows(&fx);
    assert_eq!(rows.venue.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>(), ["polymarket"]);
}

/// **`credential`'s scope check** (`CHECK (account_id IS NULL OR venue_id IS NULL)`), asked of the
/// table the engine holds: it must REFUSE a row naming both an account and a venue, and ADMIT each
/// of the three kinds §5.1 allows.
///
/// ⚠ The DDL gate cannot hold it: its drift key is `credential.check:account_id,venue_id` on both
/// sides (§3's exactly-one check names the same two columns), so deleting the DDL's check changes
/// no key and the gate stays green. This is the behavioural guard.
#[test]
fn credentials_scope_check_refuses_both_and_admits_each_kind() {
    let fx = planted();
    let account = fx.id_of("binance", "demo");
    let conn = fx.conn();
    let insert = |account_id: Option<i64>, venue: Option<&str>, name: &str| {
        conn.execute(
            "INSERT INTO credential (account_id, venue_id, field, value, name) \
             VALUES (?1, (SELECT id FROM venue WHERE name = ?2), 'F', 'v', ?3)",
            (account_id, venue, name),
        )
    };
    let both =
        insert(Some(account), Some("binance"), "check-both").expect_err("both set must refuse");
    assert!(
        both.to_string().contains("CHECK constraint failed"),
        "the scope check refuses: {both}"
    );
    insert(Some(account), None, "check-account").expect("an account-scoped row is admitted");
    insert(None, Some("okx"), "check-venue").expect("a venue-scoped row is admitted");
    insert(None, None, "check-infrastructure").expect("an infrastructure row is admitted");
}

/// **A venue-scoped key filed under a venue the roster does not hold is REFUSED by name, not filed as
/// infrastructure.** A venue-scoped `credential` row names its venue by number alone, looked up by
/// name in the INSERT itself, so a venue the `venue` table lacks would get a NULL number and no text
/// — a row naming no venue, which every reader takes for an infrastructure key. Driven through the
/// credential door a rotating grant persists through, where a refusal is an error naming the key.
#[test]
fn a_venue_scoped_key_for_a_venue_off_the_roster_is_refused_by_name() {
    let fx = Fixture::seeded();
    let key = "an-off-roster-venue-key";
    let classify = |name: &str| -> vike_secrets::Classification {
        if name == key {
            return vike_secrets::Classification {
                placement: vike_secrets::Placement::Venue("no-such-venue".to_string()),
                field: "API_KEY".to_string(),
                secret: true,
                recognised: true,
            };
        }
        crate::support::classify(name)
    };
    let err = vike_secrets::save_credentials_to_store(
        fx.dir(),
        vike_secrets::Table::Credential,
        &[(key.to_string(), "an-off-roster-value".to_string())],
        Some(&classify),
    )
    .expect_err("a key naming no roster venue must not land");
    // The door returns the store's refusal as text (`std::io::Error`), so the refusal is asked of
    // its rendering: `SchemaRefusal::VenueNotOnRoster`'s, which names the key and the venue.
    let text = err.to_string();
    assert!(
        text.contains(&format!(
            "{key}: the classifier filed this key under venue \"no-such-venue\", which this store's \
             venue table does not hold"
        )),
        "refused by name, as the venue it was filed under: {text}"
    );
    assert!(text.contains("NOTHING WAS WRITTEN"), "…saying nothing landed: {text}");
    assert!(!text.contains("an-off-roster-value"), "…never by value: {text}");
    let conn = fx.conn();
    let filed: i64 = conn
        .query_row("SELECT COUNT(*) FROM credential WHERE name = ?1", [key], |r| r.get(0))
        .expect("count");
    assert_eq!(filed, 0, "…and nothing was filed, as infrastructure or otherwise");
}
