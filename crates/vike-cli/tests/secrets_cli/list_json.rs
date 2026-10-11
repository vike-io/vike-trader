//! `list --json`: the machine rendering of the same disclosure, and where `--json` is refused.

use super::{Case, SAMPLE, stderr, stdout};

// ---- `list --json` ----------------------------------------------------------------------------

/// The machine rendering of the same disclosure: valid JSON on stdout, the store's path, every key
/// NAME — and **no value anywhere in the document**.
///
/// ⚠ The value assertion is over the RAW TEXT rather than over a field, deliberately. A field-level
/// check only proves the fields it names are clean; a machine-readable listing that grew a
/// `"values"` array, or embedded one in a message, would pass it while doing exactly the thing this
/// command exists not to do. The whole point of `list` is that its output is safe to paste.
#[test]
fn a_json_listing_carries_names_and_never_a_value() {
    let c = Case::new("list-json");
    c.write_store(SAMPLE);

    let out = c.run_raw(&["list", "--json"]);
    assert!(out.status.success(), "list --json failed: {}", stderr(&out));
    let text = stdout(&out);
    for value in ["sup3r-s3cr3t-value", "key-abcd1234", "quoted-pass"] {
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
    assert!(keys.contains(&"BINANCE_LIVE_API_KEY"), "{keys:?}");
    assert!(keys.contains(&"BINANCE_LIVE_API_SECRET"), "{keys:?}");
    assert!(keys.contains(&"OKX_DEMO_API_PASSPHRASE"), "{keys:?}");
    assert_eq!(keys.len(), 3, "every key name, and nothing else: {keys:?}");
}

/// The two renderings answer with the SAME accounts, derived from the same key names — the property
/// that keeps a machine and a person from being told different things about one store.
#[test]
fn the_json_accounts_are_the_ones_the_human_listing_shows() {
    let c = Case::new("list-json-accounts");
    c.write_store(
        "HYPERLIQUID_LIVE_API_KEY__ALT=key-alt\n\
         HYPERLIQUID_LIVE_API_SECRET__ALT=secret-alt\n\
         BINANCE_LIVE_API_KEY=key-default\n\
         BINANCE_LIVE_API_SECRET=secret-default\n",
    );

    let human = stdout(&c.run("list"));
    let out = c.run_raw(&["list", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    let accounts = doc["accounts"].as_array().expect("accounts is an array");

    assert_eq!(accounts.len(), 2, "one per account the grammar recognises: {}", stdout(&out));
    for a in accounts {
        let venue = a["venue"].as_str().expect("venue is a string");
        assert!(human.contains(venue), "the human listing names the same venue {venue}: {human}");
    }
    // The labelled one carries its label; the unlabelled one carries `null` — never the word
    // DEFAULT, which is the spelling `AccountLabel::parse` refuses.
    let labels: Vec<Option<&str>> = accounts.iter().map(|a| a["label"].as_str()).collect();
    assert!(labels.contains(&Some("ALT")), "{labels:?}");
    assert!(labels.contains(&None), "the default account's label is null: {labels:?}");
    assert!(!stdout(&out).contains("DEFAULT"), "{}", stdout(&out));
}

/// An ABSENT store is the live gate rather than a failure, and the machine shape says so with a
/// `null` — not with a sentence a caller would have to pattern-match.
#[test]
fn a_json_listing_of_an_absent_store_is_a_null_store_and_a_success() {
    let c = Case::new("list-json-absent");
    let out = c.run_raw(&["list", "--json"]);
    assert!(out.status.success(), "an absent store must not be an error: {}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    assert!(doc["store"].is_null(), "{}", stdout(&out));
    assert_eq!(doc["keys"].as_array().map(Vec::len), Some(0));
}

/// **THE `--json` DOCUMENT'S FIELD SET, PINNED.**
///
/// The suite around this reads one field at a time, which is the right shape for asserting what a
/// field MEANS and the wrong shape for noticing that the document GREW one. It grew one without
/// anybody noticing: `docs/decisions/0054`'s credential half added `kind` (`database` |
/// `absent` today; `file` too until 2026-10-07), and every test here stayed green because none of them looks at the document as a
/// whole.
///
/// This is a machine-readable contract. A consumer pattern-matching it is entitled to know when it
/// changes, and the only way to make the NEXT addition deliberate is to make it a test edit. So the
/// top-level keys are pinned as a SET, both directions:
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
    const FIELDS: [&str; 4] = ["accounts", "keys", "kind", "store"];

    let c = Case::new("list-json-shape");
    c.write_store(SAMPLE);
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

    // …and the shape holds on the ABSENT store too, which is the branch that renders a different
    // value for two of the four and would be the easy one to forget.
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
