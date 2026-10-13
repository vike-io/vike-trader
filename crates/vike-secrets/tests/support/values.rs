//! The node-key predicates and the fake secret values every fixture here writes.
//!
//! ⚠ **No test classifies a credential NAME any more.** This file held the table-driven stand-in for
//! the production name classifier the schema-2 fill took; decision 0156 item 6 deleted that
//! classifier and that fill. An account is created by the operator's act (`AccountEdit::Create`)
//! and its credentials are written against it by FIELD (`save_owned_credentials*`); a name-addressed
//! write files a DEPLOYMENT row or a node key and nothing else. What is left here is what a fixture
//! still needs: which names are node keys, and a value per name.

/// No node keys in this fixture — the `node_key` namespace is 0051's and nothing here touches it.
/// The answer for every fixture that plants no node file; a fixture that DOES plant one names its
/// own with [`node_keys_in`].
pub fn is_node_key(_key: &str) -> bool {
    false
}

/// **A node-key predicate over a fixed list of names** — the shape `database_migration.rs` and
/// `venue_setting_fold.rs` each hand-wrote (`LIVE_NODE_KEYS.contains(&key)` and `key == NODE_KEY`).
///
/// ⚠ Spelled over the fixture's OWN names rather than over `vike_model::credential_keys::
/// is_platform_key`, on purpose and by ruling (2026-09-26): the
/// test answers about the box it MEASURED, so it can still disagree with the production table.
pub fn node_keys_in<'a>(names: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
    move |key| names.contains(&key)
}

/// A fake value that is stable per key, so an assertion can prove a value did NOT come back — or
/// that a round trip DID return the right one — without any real credential existing anywhere near
/// a test file.
pub fn fake_value(key: &str) -> String {
    format!("value-for-{key}")
}

/// **[`fake_value`] with alpaca's old `SANDBOX` tier spelling folded onto `DEMO`** — the one other
/// body any file had (`scoped_read.rs`).
///
/// ⚠ **An ALIAS row carries its live twin's value**, so the value a reader hands back for the alias
/// name is the live name's. An older store holds such a row (`Fixture::plant_aged_alias`); no writer
/// files one any more.
pub fn fake_value_folding_sandbox(key: &str) -> String {
    format!("value-for-{}", key.replace("_SANDBOX_", "_DEMO_"))
}
