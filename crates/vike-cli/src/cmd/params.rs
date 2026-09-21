//! `vike-cli backtest params [--script PATH | --strategy NAME]` — **what knobs exist**, OFFLINE
//! (spec §8.5). No server, no store, no engine.
//!
//! Two sources, one verb, because the question is the same one: `--script` discovers a Rhai file's
//! `param(name, default)` calls through `vike_script::discover_params` (this is `backtest
//! --list-params`, re-homed), and `--strategy` reads the BUILT-IN roster's declaration,
//! `vike_strategy::PARAM_KEYS`.
//!
//! ⚠ **The two answers are different shapes and the `--json` document says which it is.** A Rhai
//! knob's default is an `f64` — `discover_params` returns `Vec<(String, f64)>`, so a `param()` is
//! numeric-only — while a declared strategy key carries a `vike_strategy::ParamType` and no default.
//! One document with a `source` discriminator beats two documents nobody can tell apart.
//!
//! # ⚠ Seven of the seventeen roster names have no declaration, and this says so
//!
//! `PARAM_KEYS` is exhaustive over `vike_strategy::PORTABLE_STRATEGIES` (ten names) and no further.
//! The seven in `vike_strategy::SIMULATOR_ONLY` have no row, and reporting "no tunable params" for
//! them would be FALSE — `CheapNp::from_params` reads `spot_symbol` at minimum. Each of those rows
//! carries the reason it is simulator-bound, and that reason is what is printed. Some of the ten
//! portable rows are `ParamKeys::NotEnumerated`, which carries its own reason; that is printed too.
//! The missing half §8.5 names — parameters can be SEEN and not SET — is `run --param`, a different
//! stage.

use vike_strategy::{ParamKeys, ParamType};

use crate::exit::{CliError, CmdResult};

/// What this registry can say about one strategy name. FOUR answers, never silence.
#[derive(Debug)]
pub(crate) enum Answer {
    /// Exactly the keys that name's reader looks at, each with the TOML type it accepts.
    Declared(&'static [(&'static str, ParamType)]),
    /// Deliberately not enumerated, with the reason `vike_strategy::PARAM_KEYS` records.
    NotEnumerated(&'static str),
    /// On `vike_strategy::SIMULATOR_ONLY` — no `PARAM_KEYS` row exists and "no params" would be a
    /// lie. The reason that table records is the answer.
    SimulatorOnly(&'static str),
    /// On `vike_strategy::SCRIPT_ONLY` — the name RESOLVES and its knobs are the script's own, so
    /// `--script` is the question to ask. See [`describe`].
    ScriptOnly(&'static str),
}

/// One strategy name's answer, or `None` for a name NO roster carries.
///
/// ⚠ **`vike_strategy::SCRIPT_ONLY` is consulted, and leaving it out reproduced the exact false
/// answer that table exists to end.** Its own doc says so in as many words — *"a profile naming the
/// script strategy was told it did not EXIST, which is false and sends the operator hunting for a
/// typo"* — and `backtest params --strategy rhai` was answering "no built-in strategy named 'rhai'"
/// over a roster that omitted it. It is the THIRD row-set `vike_strategy::capability` consults, and
/// it has to be the third one here for the same reason.
pub(crate) fn describe(name: &str) -> Option<Answer> {
    if let Some(keys) = vike_strategy::param_keys(name) {
        return Some(match keys {
            ParamKeys::Declared(k) => Answer::Declared(k),
            ParamKeys::NotEnumerated(why) => Answer::NotEnumerated(why),
        });
    }
    if let Some((_, why)) = vike_strategy::SIMULATOR_ONLY.iter().find(|(n, _)| *n == name) {
        return Some(Answer::SimulatorOnly(why));
    }
    vike_strategy::SCRIPT_ONLY
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, why)| Answer::ScriptOnly(why))
}

/// Every name this verb can answer for, in roster order — what a refusal names. All THREE row-sets,
/// so the roster a refusal prints is the roster [`describe`] resolves.
fn roster() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = vike_strategy::PORTABLE_STRATEGIES.to_vec();
    names.extend(vike_strategy::SIMULATOR_ONLY.iter().map(|(n, _)| *n));
    names.extend(vike_strategy::SCRIPT_ONLY.iter().map(|(n, _)| *n));
    names
}

pub(crate) fn strategy_json(name: &str, answer: &Answer) -> String {
    let params: Vec<serde_json::Value> = match answer {
        Answer::Declared(keys) => keys
            .iter()
            .map(|(k, t)| serde_json::json!({ "name": k, "type": format!("{t:?}") }))
            .collect(),
        Answer::NotEnumerated(_) | Answer::SimulatorOnly(_) | Answer::ScriptOnly(_) => Vec::new(),
    };
    let note = match answer {
        Answer::Declared(_) => serde_json::Value::Null,
        Answer::NotEnumerated(why) | Answer::SimulatorOnly(why) | Answer::ScriptOnly(why) => {
            serde_json::json!(why)
        }
    };
    let doc = serde_json::json!({
        "source": "strategy",
        "strategy": name,
        "params": params,
        "note": note,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

pub(crate) fn script_json(path: &str, params: &[(String, f64)]) -> String {
    let rows: Vec<serde_json::Value> =
        params.iter().map(|(n, d)| serde_json::json!({ "name": n, "default": d })).collect();
    let doc = serde_json::json!({ "source": "script", "script": path, "params": rows });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

pub(crate) fn run_params(
    script: Option<&str>,
    strategy: Option<&str>,
    json: bool,
) -> CmdResult<()> {
    match (script, strategy) {
        (Some(_), Some(_)) => Err(CliError::usage(
            "--script and --strategy name two different sources for the same question — give \
             exactly one",
        )),
        (None, None) => Err(CliError::usage(
            "`backtest params` needs a source: --script <s.rhai> for a script's param() knobs, or \
             --strategy NAME for a built-in strategy's declared keys",
        )),
        (Some(path), None) => run_script(path, json),
        (None, Some(name)) => run_strategy(name, json),
    }
}

fn run_script(path: &str, json: bool) -> CmdResult<()> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| CliError::failed(format!("cannot read script {path}: {e}")))?;
    let params = vike_script::discover_params(&src)
        .map_err(|e| CliError::failed(format!("rhai compile error: {e}")))?;
    if json {
        println!("{}", script_json(path, &params));
        return Ok(());
    }
    if params.is_empty() {
        println!("(no tunable params — the script declares no param(name, default) calls)");
    } else {
        for (name, default) in params {
            println!("{name} = {default}");
        }
    }
    Ok(())
}

fn run_strategy(name: &str, json: bool) -> CmdResult<()> {
    let answer = describe(name).ok_or_else(|| {
        CliError::usage(format!(
            "no built-in strategy named '{name}'. The roster is: {}",
            roster().join(", ")
        ))
    })?;
    if json {
        println!("{}", strategy_json(name, &answer));
        return Ok(());
    }
    match &answer {
        Answer::Declared(keys) => {
            for (k, t) in *keys {
                println!("{k} : {t:?}");
            }
        }
        // ⚠ NOT "(no tunable params)". That would be false, and the reason each table records is
        // exactly what an operator needs instead.
        Answer::NotEnumerated(why) => println!("{name}: keys not enumerated — {why}"),
        Answer::SimulatorOnly(why) => println!("{name}: {why}"),
        // ⚠ It RESOLVES, and its knobs are the SCRIPT's rather than a declaration — so the answer
        // names the question to ask instead. Telling an operator this name does not exist is the
        // exact false answer `vike_strategy::SCRIPT_ONLY` was written to end.
        Answer::ScriptOnly(why) => {
            println!("{name}: {why}");
            println!("  its knobs are the SCRIPT's: `vike-cli backtest params --script <s.rhai>`");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ten PORTABLE names answer with a declared key set or a declared REASON for not
    /// enumerating one. Neither is silence.
    #[test]
    fn every_portable_strategy_answers_with_keys_or_a_stated_reason() {
        for name in vike_strategy::PORTABLE_STRATEGIES {
            let answer = describe(name).unwrap_or_else(|| panic!("{name} has no answer"));
            match answer {
                Answer::Declared(keys) => assert!(!keys.is_empty(), "{name} declared an empty set"),
                Answer::NotEnumerated(why) => assert!(!why.is_empty(), "{name}"),
                // A portable name is on NEITHER of the other two row-sets — reaching one of these
                // arms means `describe` resolved the same name twice, in the wrong order.
                Answer::SimulatorOnly(_) => panic!("{name} is portable, not simulator-only"),
                Answer::ScriptOnly(_) => panic!("{name} is portable, not script-only"),
            }
        }
    }

    /// ⚠ The SIMULATOR_ONLY names have no `PARAM_KEYS` row at all, and "no params" would be a LIE.
    /// Each answers with the reason `vike_strategy::SIMULATOR_ONLY` already records.
    #[test]
    fn every_simulator_only_strategy_says_so_rather_than_reporting_no_params() {
        for (name, reason) in vike_strategy::SIMULATOR_ONLY {
            match describe(name) {
                Some(Answer::SimulatorOnly(why)) => assert_eq!(why, *reason),
                other => panic!("{name}: expected the simulator-only reason, got {other:?}"),
            }
        }
    }

    #[test]
    fn an_unknown_strategy_is_a_usage_refusal_that_names_the_roster() {
        let e = run_params(None, Some("no-such-strategy"), false).expect_err("unknown");
        assert_eq!(e.exit, crate::exit::Exit::Usage);
        assert!(e.msg.contains("no-such-strategy"), "{}", e.msg);
        assert!(e.msg.contains("buy_hold"), "it names the roster: {}", e.msg);
    }

    #[test]
    fn exactly_one_of_script_and_strategy_is_required() {
        let e = run_params(None, None, false).expect_err("neither");
        assert_eq!(e.exit, crate::exit::Exit::Usage);
        assert!(e.msg.contains("--script"), "{}", e.msg);
        assert!(e.msg.contains("--strategy"), "{}", e.msg);

        let e = run_params(Some("s.rhai"), Some("buy_hold"), false).expect_err("both");
        assert_eq!(e.exit, crate::exit::Exit::Usage);
    }

    /// ⚠ A Rhai knob's default is an `f64` — `vike_script::discover_params` returns
    /// `Vec<(String, f64)>` — so the `--json` document emits NUMBERS, never typed values. A declared
    /// STRATEGY key carries a `ParamType` instead and emits the type NAME, and the two documents say
    /// which they are so nobody writes one parser for both.
    #[test]
    fn the_json_documents_declare_which_shape_they_are() {
        let doc: serde_json::Value =
            serde_json::from_str(&strategy_json("buy_hold", &describe("buy_hold").unwrap()))
                .unwrap();
        assert_eq!(doc["source"], serde_json::json!("strategy"));
        assert_eq!(doc["strategy"], serde_json::json!("buy_hold"));
        assert!(doc["params"].is_array());
        assert!(doc["params"][0]["type"].is_string(), "a declared key carries a TYPE");

        let doc: serde_json::Value =
            serde_json::from_str(&script_json("s.rhai", &[("threshold".to_string(), 60.0)]))
                .unwrap();
        assert_eq!(doc["source"], serde_json::json!("script"));
        assert!(doc["params"][0]["default"].is_number(), "a Rhai knob's default is a NUMBER");
    }

    /// ⚠ **`rhai` RESOLVES.** `vike_strategy::SCRIPT_ONLY`'s own doc states the defect it exists to
    /// end — *"a profile naming the script strategy was told it did not EXIST, which is false and
    /// sends the operator hunting for a typo"* — and this verb reproduced it exactly, by consulting
    /// two of the three row-sets `vike_strategy::capability` consults. The answer names the script
    /// question rather than denying the name.
    #[test]
    fn the_script_only_name_resolves_rather_than_being_denied() {
        for (name, reason) in vike_strategy::SCRIPT_ONLY {
            match describe(name) {
                Some(Answer::ScriptOnly(why)) => assert_eq!(why, *reason),
                other => panic!("{name}: expected the script-only reason, got {other:?}"),
            }
            assert!(roster().contains(name), "the refusal roster omits `{name}`");
        }
        // ...and it is NOT refused: a refusal is what the defect looked like.
        run_params(None, Some("rhai"), false).expect("`rhai` is a name this registry resolves");
    }

    /// The roster a refusal prints is the roster `describe` resolves — all three row-sets. A name
    /// answerable but unadvertised sends an operator hunting exactly as a denied one does.
    #[test]
    fn the_refusal_roster_and_the_resolver_cover_the_same_names() {
        for name in roster() {
            assert!(describe(name).is_some(), "`{name}` is advertised and does not resolve");
        }
        let e = run_params(None, Some("no-such-strategy"), false).expect_err("unknown");
        for name in roster() {
            assert!(e.msg.contains(name), "the refusal omits `{name}`: {}", e.msg);
        }
    }
}
