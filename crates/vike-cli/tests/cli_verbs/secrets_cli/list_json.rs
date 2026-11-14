//! `list --json`: the machine rendering of the same disclosure, and where `--json` is refused.

use super::list::section;
use super::{Case, SAMPLE, SAMPLE_ACCOUNT, stderr, stdout};

// ---- `list --json` ----------------------------------------------------------------------------

/// The machine rendering of the same disclosure: valid JSON on stdout, the store's path, every key
/// NAME — the deployment's and an account's — and **no value anywhere in the document**.
///
/// ⚠ The value assertion is over the RAW TEXT rather than over a field, deliberately. A field-level
/// check only proves the fields it names are clean; a machine-readable listing that grew a
/// `"values"` array, or embedded one in a message, would pass it while doing exactly the thing this
/// command exists not to do. The whole point of `list` is that its output is safe to paste.
#[test]
fn a_json_listing_carries_names_and_never_a_value() {
    let c = Case::new("list-json");
    c.write_store(SAMPLE);
    let id = c.seed_accounts(&[SAMPLE_ACCOUNT])[0];

    let out = c.run_raw(&["list", "--json"]);
    assert!(out.status.success(), "list --json failed: {}", stderr(&out));
    let text = stdout(&out);
    for value in
        ["sup3r-s3cr3t-value", "key-abcd1234", "quoted-pass", "acct-key-5678", "acct-s3cr3t"]
    {
        assert!(!text.contains(value), "a credential VALUE reached stdout: {text}");
    }

    let doc: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("`list --json` must print ONE JSON document: {e}\n{text}"));
    assert_eq!(
        doc["store"].as_str(),
        Some(c.db().display().to_string().as_str()),
        "the document names the store it read: {text}"
    );
    let keys: Vec<&str> = doc["keys"]
        .as_array()
        .expect("keys is an array")
        .iter()
        .map(|k| k.as_str().expect("every key is a string"))
        .collect();
    assert!(keys.contains(&"TARDIS_API_KEY"), "{keys:?}");
    assert!(keys.contains(&"VIKE_ALERT_TELEGRAM_TOKEN"), "{keys:?}");
    assert!(keys.contains(&"DATABENTO_API_KEY"), "{keys:?}");
    for field in ["API_KEY", "API_SECRET"] {
        let name = vike_secrets::account_credential_name("binance", id, field);
        assert!(keys.contains(&name.as_str()), "the account's {field}, by its row name: {keys:?}");
    }
    assert_eq!(keys.len(), 5, "every key name, and nothing else: {keys:?}");
}

/// **The two renderings answer with the SAME accounts and the SAME names** — the account rows, the
/// names filed against each, and the names no account owns — the property that keeps a machine and
/// a person from being told different things about one store.
///
/// The human block is rebuilt from the document line for line, so a row, a label, an `INACTIVE`
/// mark or a name one rendering carries and the other drops fails here. Beside that, what each
/// cell MEANS: the accounts are the ROWS (a keyless and an inactive one included), an unlabelled
/// row's label is `null` — never the word DEFAULT, which is the spelling `AccountLabel::parse`
/// refuses — and the names are the ones the store filed against each row.
#[test]
fn the_json_accounts_are_the_ones_the_human_listing_shows() {
    let c = Case::new("list-json-accounts");
    let ids = c.seed_accounts(&[
        ("hyperliquid", "live", Some("ALT"), &[("PRIVATE_KEY", "key-alt")]),
        ("binance", "live", None, &[("API_KEY", "key-default"), ("API_SECRET", "secret-default")]),
        ("okx", "demo", Some("SPARE"), &[]),
    ]);
    let (binance, spare) = (ids[1], ids[2]);
    vike_secrets::edit_account_in_unjournalled(
        c.dir(),
        vike_secrets::AccountEdit::SetActive { id: spare, active: false },
    )
    .expect("deactivate the SPARE row");
    c.write_store("TARDIS_API_KEY=key-tardis\n");

    let human = stdout(&c.run("list"));
    let out = c.run_raw(&["list", "--json"]);
    assert!(out.status.success(), "list --json failed: {}", stderr(&out));
    let text = stdout(&out);
    let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    let accounts = doc["accounts"].as_array().expect("accounts is an array");
    assert_eq!(accounts.len(), 3, "one per ROW of the account table: {text}");

    // The human account block, rebuilt from the document.
    let mut rebuilt = Vec::new();
    for a in accounts {
        let id = a["id"].as_i64().expect("id is a number");
        let venue = a["venue"].as_str().expect("venue is a string");
        let tier = a["tier"].as_str().expect("tier is a word");
        let label = a["label"].as_str().unwrap_or("(default)");
        let active = a["active"].as_bool().expect("active is a boolean");
        let inactive = if active { "" } else { "  INACTIVE" };
        rebuilt.push(format!("  #{id} {venue}/{tier} {label}{inactive}"));
        let names = a["keys"].as_array().expect("an account's keys is an array");
        if names.is_empty() {
            rebuilt.push("      (no credential stored yet)".to_string());
        }
        for n in names {
            rebuilt.push(format!("      {}", n.as_str().expect("a key is a string")));
        }
    }
    assert_eq!(
        section(&human, "account(s) in the account table:"),
        rebuilt,
        "the human listing shows the document's accounts: {human}\n{text}"
    );
    let as_lines = |field: &str| -> Vec<String> {
        doc[field]
            .as_array()
            .unwrap_or_else(|| panic!("{field} is an array"))
            .iter()
            .map(|k| format!("  {}", k.as_str().expect("a key is a string")))
            .collect()
    };
    assert_eq!(section(&human, "secret(s):"), as_lines("keys"), "{human}\n{text}");
    assert_eq!(
        section(&human, "name(s) no account owns"),
        as_lines("unowned_keys"),
        "{human}\n{text}"
    );

    // …and what the cells MEAN.
    let labels: Vec<Option<&str>> = accounts.iter().map(|a| a["label"].as_str()).collect();
    assert_eq!(labels, vec![Some("ALT"), None, Some("SPARE")], "the rows, by id: {text}");
    assert!(accounts[1]["label"].is_null(), "the unlabelled row's label is null: {text}");
    let tiers: Vec<&str> = accounts.iter().filter_map(|a| a["tier"].as_str()).collect();
    assert_eq!(tiers, vec!["live", "live", "demo"], "each row's OWN tier word: {text}");
    assert_eq!(accounts[2]["active"].as_bool(), Some(false), "{text}");
    assert_eq!(accounts[2]["keys"].as_array().map(Vec::len), Some(0), "{text}");
    let binance_keys: Vec<&str> = accounts[1]["keys"]
        .as_array()
        .expect("an account's keys is an array")
        .iter()
        .filter_map(|k| k.as_str())
        .collect();
    assert_eq!(
        binance_keys,
        vec![
            vike_secrets::account_credential_name("binance", binance, "API_KEY"),
            vike_secrets::account_credential_name("binance", binance, "API_SECRET"),
        ],
        "the names the store filed against the row: {text}"
    );
    assert_eq!(doc["unowned_keys"], serde_json::json!(["TARDIS_API_KEY"]), "{text}");
    assert!(!text.contains("DEFAULT"), "{text}");
    for value in ["key-alt", "key-default", "secret-default", "key-tardis"] {
        assert!(!text.contains(value) && !human.contains(value), "a VALUE reached stdout");
    }
}

/// An ABSENT store is the live gate rather than a failure, and the machine shape says so with a
/// `null` — not with a sentence a caller would have to pattern-match. No store, no account table:
/// no account and no name.
#[test]
fn a_json_listing_of_an_absent_store_is_a_null_store_and_a_success() {
    let c = Case::new("list-json-absent");
    let out = c.run_raw(&["list", "--json"]);
    assert!(out.status.success(), "an absent store must not be an error: {}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    assert!(doc["store"].is_null(), "{}", stdout(&out));
    assert_eq!(doc["keys"].as_array().map(Vec::len), Some(0));
    assert_eq!(doc["accounts"].as_array().map(Vec::len), Some(0));
    assert_eq!(doc["unowned_keys"].as_array().map(Vec::len), Some(0));
}

/// **THE `--json` DOCUMENT'S FIELD SET, PINNED** — the top level, and each account object.
///
/// The suite around this reads one field at a time, which is the right shape for asserting what a
/// field MEANS and the wrong shape for noticing that the document GREW one. It grew one without
/// anybody noticing: `docs/decisions/0054`'s credential half added `kind` (`database` |
/// `absent` today; `file` too until 2026-10-07), and every test here stayed green because none of them looks at the document as a
/// whole. (`unowned_keys` and the account objects' shape arrived with decision 0156 item 6, when the
/// accounts became the account table's ROWS rather than a reading of the key names.)
///
/// This is a machine-readable contract. A consumer pattern-matching it is entitled to know when it
/// changes, and the only way to make the NEXT addition deliberate is to make it a test edit. So the
/// keys are pinned as a SET, both directions:
///
/// * a field ADDED shows up here rather than in somebody's parser three weeks later;
/// * a field REMOVED is caught too, which is the half a "contains these keys" assertion would miss
///   and the half that actually breaks a consumer.
///
/// ⚠ It pins the NAMES, not the values — the meaning of each is asserted by the tests above, and
/// duplicating those assertions here would make this file the second authority on a question that
/// already has one.
#[test]
fn the_json_document_has_exactly_these_top_level_fields() {
    /// Every top-level key `list --json` prints. Adding one is an API change: add it here, in this
    /// order, with the tests that say what it means.
    const FIELDS: [&str; 5] = ["accounts", "keys", "kind", "store", "unowned_keys"];
    /// Every key of one `accounts` entry — the same rule.
    const ACCOUNT_FIELDS: [&str; 6] = ["active", "id", "keys", "label", "tier", "venue"];

    let c = Case::new("list-json-shape");
    c.write_store(SAMPLE);
    c.seed_accounts(&[SAMPLE_ACCOUNT]);
    let out = c.run_raw(&["list", "--json"]);
    assert!(out.status.success(), "list --json failed: {}", stderr(&out));
    let text = stdout(&out);
    let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");

    let mut found: Vec<&str> =
        doc.as_object().expect("the document is an object").keys().map(String::as_str).collect();
    found.sort_unstable();
    assert_eq!(
        found,
        FIELDS.to_vec(),
        "`list --json`'s field set changed. If that was deliberate, update FIELDS and add a test \
         saying what the new field MEANS; if it was not, this is an API change a consumer would \
         have found for you.\n{text}"
    );
    let accounts = doc["accounts"].as_array().expect("accounts is an array");
    assert_eq!(accounts.len(), 1, "the fixture's one account: {text}");
    for a in accounts {
        let mut found: Vec<&str> =
            a.as_object().expect("an account is an object").keys().map(String::as_str).collect();
        found.sort_unstable();
        assert_eq!(
            found,
            ACCOUNT_FIELDS.to_vec(),
            "an account object's field set changed — the same API change, one level down.\n{text}"
        );
    }

    // …and the shape holds on the ABSENT store too, which is the branch that renders a different
    // value for most of them and would be the easy one to forget.
    let bare = Case::new("list-json-shape-absent");
    let out = bare.run_raw(&["list", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    let mut found: Vec<&str> =
        doc.as_object().expect("the document is an object").keys().map(String::as_str).collect();
    found.sort_unstable();
    assert_eq!(found, FIELDS.to_vec(), "the absent-store document must carry the same fields");
}

/// `kind` says WHICH KIND of store answered: `database`, or `absent` with no store. (It said `file`
/// for the credential FILE store until that store was removed on 2026-10-07.)
///
/// The field exists because `store` is a path either way, so a consumer reading it can no longer
/// tell whether the location is something it may `cat`. `docs/decisions/0054`'s constraint 2 is that
/// `sqlite3` is not installed on the live box and an operator reads the store with `cat` today; the
/// word is the hint, made explicit rather than smuggled into a path's extension.
#[test]
fn the_json_kind_names_the_store_that_answered() {
    let c = Case::new("list-json-kind");
    c.write_store(SAMPLE);
    let out = c.run_raw(&["list", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    assert_eq!(doc["kind"].as_str(), Some("database"), "{}", stdout(&out));

    let bare = Case::new("list-json-kind-absent");
    let out = bare.run_raw(&["list", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    assert_eq!(doc["kind"].as_str(), Some("absent"), "{}", stdout(&out));
    assert!(doc["store"].is_null(), "…and `store` stays null beside it");
}

/// `--json` is refused on the subcommands it would mean nothing on, rather than being ignored: a
/// flag the operator typed and the program dropped is how somebody comes to believe they asked for
/// a shape they did not get. `path`'s product is three lines a human reads when something is
/// already broken.
#[test]
fn json_is_refused_where_it_would_mean_nothing() {
    let c = Case::new("list-json-refused");
    // `path` is the one other read verb (`template` went with the credential FILE store).
    let out = c.run_raw(&["path", "--json"]);
    assert!(!out.status.success(), "`secrets path --json` must be refused");
    assert!(
        stderr(&out).contains("--json applies to `list` only"),
        "and it must say so: {}",
        stderr(&out)
    );
}
