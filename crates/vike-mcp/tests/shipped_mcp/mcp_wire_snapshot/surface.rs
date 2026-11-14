//! What a client reads before it does anything: `initialize` and the `instructions`, `tools/list`,
//! `resources/*`, `prompts/*`.

use serde_json::{Value, json};

use super::harness::{Golden, Session, World};

/// Every scoped launch these goldens pin: a tag and the argv that selects it.
const PROFILES: [(&str, &[&str]); 4] = [
    ("full", &[]),
    ("read-only", &["--profile", "read-only"]),
    ("offline", &["--profile", "offline"]),
    ("deny-submit_order", &["--deny-tool", "submit_order"]),
];

/// `initialize` for each version a client sends, and each profile's `instructions`.
pub(super) fn initialize() -> Vec<Golden> {
    let versions = [
        ("absent", json!({})),
        ("not-a-string", json!({ "protocolVersion": 20241105 })),
        ("2024-11-05", json!({ "protocolVersion": "2024-11-05" })),
        ("2025-06-18", json!({ "protocolVersion": "2025-06-18" })),
        ("2025-11-25", json!({ "protocolVersion": "2025-11-25" })),
        // What Claude Code sends on stdio since 2.1.292: a probe, on a revision with no handshake.
        ("2026-07-28", json!({ "protocolVersion": "2026-07-28" })),
        ("unknown", json!({ "protocolVersion": "1999-01-01" })),
    ];
    let mut out = Vec::new();
    for (tag, params) in versions {
        let mut session = Session::start(format!("initialize.version-{tag}"), World::empty(), &[]);
        session.rpc("initialize", params);
        let doc = session.finish().what("`initialize` for one `protocolVersion`, full profile");
        out.extend(doc.golden_with_instructions("full"));
    }
    // `delete_series` is the one denial that changes the text: its clause goes with the tool.
    let denied_delete: (&str, &[&str]) = ("deny-delete_series", &["--deny-tool", "delete_series"]);
    for (tag, argv) in PROFILES.into_iter().chain([denied_delete]) {
        let mut session = Session::start(format!("initialize.profile-{tag}"), World::empty(), argv);
        session.rpc("initialize", json!({ "protocolVersion": "2025-06-18" }));
        out.extend(
            session
                .finish()
                .what("`initialize` under one scope: the `instructions` are scoped with the roster")
                .golden_with_instructions(tag),
        );
    }
    out
}

/// `tools/list` per profile, whole: the tools, their order, schemas and annotations.
pub(super) fn tools_list() -> Vec<Golden> {
    PROFILES
        .into_iter()
        .map(|(tag, argv)| {
            let mut session = Session::start(format!("tools_list.{tag}"), World::empty(), argv);
            session.rpc("tools/list", json!({}));
            session
                .finish()
                .what("`tools/list` under one scope, whole: names, order, schemas, annotations")
                .golden()
        })
        .collect()
}

/// `resources/list` and every way `resources/read` answers with no node and no run behind it.
pub(super) fn resources() -> Vec<Golden> {
    PROFILES
        .into_iter()
        .map(|(tag, argv)| {
            let mut session = Session::start(format!("resources.{tag}"), World::empty(), argv);
            session.rpc("resources/list", json!({}));
            for params in [
                json!({ "uri": "vike://backtest/last" }),
                json!({ "uri": "vike://node/snapshot" }),
                json!({ "uri": "vike://nope" }),
                json!({}),
            ] {
                session.rpc("resources/read", params);
            }
            session.finish().what("`resources/list` and `resources/read` under one scope").golden()
        })
        .collect()
}

/// `prompts/list`, and `prompts/get` with and without arguments, under the profiles that change it.
pub(super) fn prompts() -> Vec<Golden> {
    let gets: [(&str, Value); 3] = [
        ("backtest_a_strategy", json!({ "idea": "buy the dip" })),
        ("enable_a_venue", json!({ "venue": "binance" })),
        ("triage_a_stuck_order", json!({ "order_client_id": "c-42" })),
    ];
    PROFILES[..3]
        .iter()
        .map(|(tag, argv)| {
            let mut session = Session::start(format!("prompts.{tag}"), World::empty(), argv);
            session.rpc("prompts/list", json!({}));
            for (name, arguments) in &gets {
                session.rpc("prompts/get", json!({ "name": name, "arguments": arguments }));
                session.rpc("prompts/get", json!({ "name": name }));
            }
            session.rpc("prompts/get", json!({ "name": "no_such_prompt" }));
            session.rpc("prompts/get", json!({}));
            session.finish().what("`prompts/list` and `prompts/get`, full text").golden()
        })
        .collect()
}
