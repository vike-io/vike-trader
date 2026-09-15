//! `vike-cli backtest strategies` — **what the COMPUTE DAEMON can run**, over the wire (spec §8.3).
//!
//! # ⚠ It opens a socket, and the stage table says it does not
//!
//! `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`'s §17 stage-4 row reads
//! "Artifact-only; no daemon work", and §6's opener says the reading verbs open no socket. This verb
//! is in §8, not §6, and §8.3 is explicit about why: the roster reaches a shell today only through
//! the engine binary's own `--list`, which answers about the LOCAL build, not the server's. An
//! operator with `vike-cli` and a remote daemon cannot enumerate what `--strategy` may name — even
//! though the wire verb that answers it is already served. Building this offline would rebuild the
//! defect.
//!
//! # What cannot be linked, and what that buys
//!
//! `vike_backtest::harness::STRATEGIES` sits behind that crate's `hist-replay` feature, and a normal
//! `vike-backtest` edge would drag `vike-exec` into a CLI whose identity is being light and
//! DataFusion-free. So the roster arrives as `vike_datahub_client::DatahubClient::list_strategies`,
//! which is already written, already served by `vike_backtest::compute_server`, and already exposed
//! through MCP's `tool_list_strategies` — the ONE missing piece was the shell-facing verb.
//!
//! `vike_datahub_client::proto`'s `plane_of` puts `Request::ListStrategies` on `Plane::Compute`
//! (:7880, ruling 7) and `required_scope` puts it under `VerbScope::Observe` — it returns a
//! compile-time const roster, not store contents — so a key-less dial is legal and the authenticated
//! one negotiates the weaker scope. `backtest run` is Control by contrast, because it compiles
//! client-supplied Rhai on the server.
//!
//! # The address ladder is `crate::cmd::backtest::resolve_addr` and nothing else
//!
//! `--addr` → `config.backtest_addr` → `vike_config::DEFAULT_BACKTEST_ADDR`, folded in ONE function
//! that `crates/vike-cli/src/cmd/study.rs` already calls. Two copies could disagree about a blank
//! rung and aim one client at the wrong port.

use vike_datahub_client::{DatahubClient, Scope};

use crate::cmd::runs::Ctx;
use crate::exit::{CliError, CmdResult};

/// `{ addr, count, strategies }` — the roster and the daemon it came FROM, because "which server
/// answered" is half the question the moment more than one exists.
pub(crate) fn strategies_json(addr: &str, names: &[String]) -> String {
    let doc = serde_json::json!({
        "addr": addr,
        "count": names.len(),
        "strategies": names,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

pub(crate) fn run_strategies(ctx: &Ctx<'_>, cli_addr: Option<&str>, json: bool) -> CmdResult<()> {
    let addr = crate::cmd::backtest::resolve_addr(cli_addr, ctx.configured_addr);
    let mut client = match ctx.keys {
        Some(k) => DatahubClient::connect_authed(&addr, k, Scope::Observe),
        None => DatahubClient::connect(&addr),
    }
    .map_err(|e| {
        CliError::connect(format!(
            "cannot connect to the backtest daemon at {addr}: {e} (start it with \
             `vike-backend backtest --addr`)"
        ))
    })?;
    let names = client.list_strategies()?;
    if json {
        println!("{}", strategies_json(&addr, &names));
    } else {
        // ⚠ ONE NAME PER LINE and nothing else — the same shape the engine's own `--list` prints,
        // so a script that piped one can pipe the other. The address goes in the `--json` document,
        // never on stdout in the plain mode.
        for name in &names {
            println!("{name}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::backtest::resolve_addr;

    /// The ladder, all three rungs. ⚠ It is NOT a copy: this asserts
    /// `crate::cmd::backtest::resolve_addr`, the ONE implementation `crates/vike-cli/src/cmd/study.rs`
    /// also calls, so the three verbs that dial the compute daemon cannot disagree about a blank
    /// rung and aim one client at the wrong port.
    #[test]
    fn the_address_ladder_is_cli_then_configured_then_default() {
        assert_eq!(resolve_addr(Some("1.2.3.4:9"), Some("5.6.7.8:9")), "1.2.3.4:9");
        assert_eq!(resolve_addr(None, Some("5.6.7.8:9")), "5.6.7.8:9");
        assert_eq!(resolve_addr(None, None), vike_config::DEFAULT_BACKTEST_ADDR);
        // A blank rung is skipped, not honoured — an `Environment=` line that set nothing must not
        // send the client at an empty address.
        assert_eq!(resolve_addr(Some("  "), Some("5.6.7.8:9")), "5.6.7.8:9");
        assert_eq!(resolve_addr(Some("  "), Some("  ")), vike_config::DEFAULT_BACKTEST_ADDR);
    }

    #[test]
    fn the_json_document_is_the_roster_and_the_address_it_came_from() {
        let doc: serde_json::Value =
            serde_json::from_str(&strategies_json("127.0.0.1:7880", &["buy_hold".to_string()]))
                .unwrap();
        assert_eq!(doc["addr"], serde_json::json!("127.0.0.1:7880"));
        assert_eq!(doc["strategies"], serde_json::json!(["buy_hold"]));
        assert_eq!(doc["count"], serde_json::json!(1));
    }
}
