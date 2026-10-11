//! A policy `set_setting` against a real node: the old value it previews and the row it lands.

use super::*;

/// The one policy ceiling [`a_policy_write_previews_the_nodes_old_value_and_lands_on_its_token`]
/// writes — a risk ceiling, the key class the deleted retype was demanded for.
const CEILING_KEY: &str = "policy.max_notional_per_order";

/// What the node's settings DATABASE holds for the ceiling — the node's truth, read off the store
/// rather than off anything the tool answered.
fn stored_ceiling(dir: &std::path::Path) -> Option<String> {
    let source = vike_secrets::read_settings_in(dir).expect("read the node's settings database");
    let rows = &source.rows().expect("rows").settings;
    rows.iter()
        .find(|r| r.section == "policy" && r.key == "max_notional_per_order")
        .map(|r| r.value.clone())
}

/// **A policy `set_setting` against a REAL node: the preview shows the node's OWN old value, and
/// the preview token alone confirms it** (`docs/decisions/0086` point 7 — no `policy_confirm`, no
/// retype).
///
/// The pure tests beside [`Server::call_tool`] can only prove the `change` field's SHAPE — with no
/// node there is no old value to show. This is the half that proves the value is READ from the node
/// the write is aimed at, and that the preview → token → write path lands the row with nothing else
/// asked for. The old value the preview must show is read through the node's own `SettingsShow`, so
/// the assertion is about the line and not about how the node renders a float.
#[test]
fn a_policy_write_previews_the_nodes_old_value_and_lands_on_its_token() {
    let store = tempfile::tempdir().expect("tempdir");
    vike_secrets::plant_settings_rows(
        store.path(),
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "policy".into(),
                key: "max_notional_per_order".into(),
                value: "100".into(),
            }],
            venue: Vec::new(),
        },
    )
    .expect("seed the node's settings database");
    let (_mount, node) = spawn_node_with_settings(store.path());
    // The module doc's first measured fact: a paper core that has folded nothing never publishes,
    // and the preview and the confirm each read the node's frame for its account epoch.
    seed_order(node);
    let mut s = server_at(node);
    let old = vike_tradehub_client::settings_show(node, OBSERVE_KEY.as_bytes())
        .expect("the node serves its settings")
        .rows
        .into_iter()
        .find(|r| r.key == CEILING_KEY)
        .expect("the node carries the ceiling row")
        .value;

    let args = json!({ "key": CEILING_KEY, "value": "250" });
    let pv =
        s.call_tool("set_setting", &args).expect("a policy write PREVIEWS — nothing refuses it");
    assert_eq!(pv["will_execute"], json!(false), "{pv}");
    assert_eq!(
        pv["change"]["old"],
        json!(old),
        "the node's CURRENT value, read off the node: {pv}"
    );
    assert_eq!(pv["change"]["new"], json!("250"), "{pv}");
    assert_eq!(pv["change"]["line"], json!(format!("{CEILING_KEY}: {old} → 250")), "{pv}");
    assert_eq!(stored_ceiling(store.path()).as_deref(), Some("100"), "a preview writes nothing");

    let mut confirming = args.clone();
    confirming["confirm"] = json!(true);
    confirming["preview_token"] = pv["preview_token"].clone();
    let ok = s.call_tool("set_setting", &confirming).expect("the token alone confirms it");
    assert_eq!(ok["outcome"], "accepted", "{ok}");
    assert_eq!(ok["restart_required"], json!(true), "policy is never hot: {ok}");
    assert_eq!(
        stored_ceiling(store.path()).as_deref(),
        Some("250"),
        "the confirmed write lands in the node's settings database: {ok}"
    );
}
