//! `vike_model::host_build::state_signature` on planted files: the shipped makers' JSON state shapes
//! (and `json!`, a turbofish, an unused parameter) move onto `StrategyState` exactly as the makers did,
//! a body of any other shape draws a hint on its line and is not touched, a second pass finds nothing,
//! and only a `Strategy` impl is read. Whether the result BUILDS is the builder's lane test
//! (`crates/vike-strategy-builder/tests/build_errors.rs`'s
//! `a_file_on_the_json_state_signature_draws_the_migrate_hint_and_its_migration_builds`).

use vike_model::host_build::moved_name::source_hints;
use vike_model::host_build::source_edits::{SourceEdits, apply_edits};
use vike_model::host_build::state_signature::{state_edits, state_hint};

/// The file `state_edits` turns `source` into.
fn migrated(source: &str) -> String {
    apply_edits(source, &state_edits(source).edits)
}

/// The shapes `strategies/market_making/spread_maker.rs` had before `ABI_VERSION` 15.
const MAKER: &str = r#"use vike_model::{OrderBroker, Strategy};

pub struct Maker {
    seen: usize,
}

impl<B: OrderBroker> Strategy<B> for Maker {
    fn save_state(&self) -> Option<serde_json::Value> {
        let dto = Saved { v: 1, seen: self.seen };
        serde_json::to_value(&dto).ok()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        // serde_json::from_value(state.clone()) in a comment stays
        let dto: Saved = match serde_json::from_value(state.clone()) {
            Ok(dto) => dto,
            Err(err) => {
                tracing::warn!(%err, "serde_json::Value in a string stays");
                return;
            }
        };
        self.seen = dto.seen;
    }
}
"#;

#[test]
fn the_makers_shapes_move_onto_the_state_type_as_the_makers_did() {
    let expected = MAKER
        .replace(
            "fn save_state(&self) -> Option<serde_json::Value>",
            "fn save_state(&self) -> Option<vike_model::StrategyState>",
        )
        .replace("serde_json::to_value(&dto).ok()", "vike_model::StrategyState::of(&dto).ok()")
        .replace("state: &serde_json::Value)", "state: &vike_model::StrategyState)")
        .replace("match serde_json::from_value(state.clone())", "match state.read()");
    assert_eq!(migrated(MAKER), expected);
    assert!(expected.contains("// serde_json::from_value(state.clone()) in a comment stays"));
    assert!(state_edits(MAKER).hints.is_empty(), "{:?}", state_edits(MAKER).hints);
}

#[test]
fn json_a_turbofish_and_an_unused_parameter_are_carried_too() {
    let source = "impl<B: vike_model::OrderBroker> vike_model::Strategy<B> for Counter {\n\
                  \x20   fn save_state(&self) -> Option<::serde_json::Value> {\n\
                  \x20       Some(serde_json::json!({ \"n\": self.n }))\n\
                  \x20   }\n\
                  \x20   fn load_state(&mut self, _state: &Value) {}\n\
                  }\n\
                  impl<B: vike_model::OrderBroker> vike_model::Strategy<B> for Typed {\n\
                  \x20   fn load_state(&mut self, s: &serde_json::Value) {\n\
                  \x20       if let Ok(n) = serde_json::from_value::<Vec<u64>>(s.to_owned()) { self.n = n; }\n\
                  \x20   }\n\
                  }\n";
    let expected = "impl<B: vike_model::OrderBroker> vike_model::Strategy<B> for Counter {\n\
                    \x20   fn save_state(&self) -> Option<vike_model::StrategyState> {\n\
                    \x20       Some(vike_model::StrategyState::from_json_value(&serde_json::json!({ \"n\": self.n })))\n\
                    \x20   }\n\
                    \x20   fn load_state(&mut self, _state: &vike_model::StrategyState) {}\n\
                    }\n\
                    impl<B: vike_model::OrderBroker> vike_model::Strategy<B> for Typed {\n\
                    \x20   fn load_state(&mut self, s: &vike_model::StrategyState) {\n\
                    \x20       if let Ok(n) = s.read::<Vec<u64>>() { self.n = n; }\n\
                    \x20   }\n\
                    }\n";
    assert_eq!(migrated(source), expected);
}

/// **An unrecognized body is left exactly as it is**, with a hint naming its line and the new
/// signature: a `save_state` that builds its document by hand, a `load_state` that indexes it.
#[test]
fn a_body_of_another_shape_draws_a_hint_on_its_line_and_is_not_touched() {
    let source = "impl<B: OrderBroker> Strategy<B> for Hand {\n\
                  \x20   fn save_state(&self) -> Option<serde_json::Value> {\n\
                  \x20       let mut v = serde_json::json!({});\n\
                  \x20       v[\"n\"] = self.n.into();\n\
                  \x20       Some(v)\n\
                  \x20   }\n\
                  \x20   fn load_state(&mut self, state: &serde_json::Value) {\n\
                  \x20       self.n = state[\"n\"].as_u64().unwrap_or(0);\n\
                  \x20   }\n\
                  }\n";
    let found = state_edits(source);
    assert_eq!(found.edits, Vec::new(), "nothing is edited");
    assert_eq!(migrated(source), source);
    assert_eq!(found.hints.len(), 2, "{:?}", found.hints);
    assert!(
        found.hints[0]
            .starts_with("line 2: `save_state` returns `Option<vike_model::StrategyState>`")
            && found.hints[0]
                .contains("write `fn save_state(&self) -> Option<vike_model::StrategyState>`"),
        "{}",
        found.hints[0]
    );
    assert!(
        found.hints[1].starts_with("line 7: `load_state` takes `&vike_model::StrategyState`")
            && found.hints[1]
                .contains("write `fn load_state(&mut self, state: &vike_model::StrategyState)`"),
        "{}",
        found.hints[1]
    );
}

#[test]
fn a_second_pass_finds_nothing() {
    for source in [MAKER.to_string(), migrated(MAKER)] {
        let once = migrated(&source);
        assert_eq!(state_edits(&once), SourceEdits::default(), "{once}");
        assert_eq!(state_hint(&once), None);
    }
}

/// An inherent helper of the same name is the file's own: only a `Strategy` impl is read.
#[test]
fn an_inherent_helper_of_the_same_name_is_not_touched() {
    let source = "impl Ledger {\n\
                  \x20   fn save_state(&self) -> Option<serde_json::Value> { serde_json::to_value(&self.0).ok() }\n\
                  }\n";
    assert_eq!(state_edits(source), SourceEdits::default());
    assert_eq!(state_hint(source), None);
}

/// The builder's line names the change and the command, for an edit or a hint alike, and only for a
/// file still on the JSON signature.
#[test]
fn the_builders_hint_names_the_change_and_the_migrate_command() {
    let hint = state_hint(MAKER).expect("the JSON signature draws it");
    assert!(
        hint.contains("`vike_model::StrategyState` (msgpack) since ABI 15")
            && hint.contains("`vike-cli strategy migrate <file>`"),
        "{hint}"
    );
    assert_eq!(source_hints(MAKER), vec![hint.clone()], "the builder prints it after rustc's text");
    let by_hand = "impl<B: OrderBroker> Strategy<B> for H {\n\
                   \x20   fn load_state(&mut self, state: &serde_json::Value) { self.f(state) }\n\
                   }\n";
    assert_eq!(state_hint(by_hand), Some(hint));
    assert_eq!(state_hint(&migrated(MAKER)), None);
}
