//! Wire the user's own local Claude to the vike MCP server — the ChatPane "Connect to Claude"
//! helper behind the non-default `mcp` feature. Successor of the retired `vike-mcp` crate's
//! `connect` module (Phase B of the two-MCP-server consolidation, #842): the spawned server is now
//! the `vike-cli` binary's `mcp` subcommand, whose run/list tools go over a RUNNING `vike-datahub`
//! server (`--addr`, default `127.0.0.1:7878`) instead of an in-process DataFusion store — so the
//! generated command carries the datahub address, never a `VIKE_HIST_STORE` path. Pure +
//! unit-tested: build the server entry and emit the `claude mcp add …` line for Claude Code. (The
//! old module's Claude-Desktop config-JSON merge helper was dropped with the crate — the Studio
//! only ever surfaced the copy-pasteable command.)

/// The pieces of a `claude mcp add` invocation: the executable and its argv tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerEntry {
    pub command: String,
    pub args: Vec<String>,
}

/// Build the entry for the `vike-cli` binary's `mcp` subcommand. `datahub_addr`, when given, is
/// passed as `--addr` (the vike-datahub server the run/list tools dial — the Studio threads its
/// Remote backend's address here); absent = the spawned server falls back to its own default
/// (`127.0.0.1:7878`, the SSH-tunnelled port of the deploy runbook). The optional vike-tradehub
/// node keys (`VIKE_TRADEHUB_*_KEY`) are the operator's process-env concern — never something the
/// Studio writes into a generated command.
pub fn server_entry(exe: &str, datahub_addr: Option<&str>) -> McpServerEntry {
    let mut args = vec!["mcp".to_string()];
    if let Some(addr) = datahub_addr {
        args.push("--addr".to_string());
        args.push(addr.to_string());
    }
    McpServerEntry { command: exe.to_string(), args }
}

/// The copy-pasteable `claude mcp add …` command for Claude Code.
pub fn claude_code_add_command(name: &str, entry: &McpServerEntry) -> String {
    let mut parts = vec![format!("claude mcp add {name}"), "--".to_string()];
    parts.push(entry.command.clone());
    parts.extend(entry.args.iter().cloned());
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_entry_spawns_the_mcp_subcommand() {
        let e = server_entry("/opt/vike-cli", None);
        assert_eq!(e.command, "/opt/vike-cli");
        assert_eq!(e.args, vec!["mcp".to_string()], "no --addr when no datahub address given");
    }

    #[test]
    fn server_entry_carries_the_datahub_addr() {
        let e = server_entry("/opt/vike-cli", Some("<host>:7878"));
        assert_eq!(
            e.args,
            vec!["mcp".to_string(), "--addr".to_string(), "<host>:7878".to_string()]
        );
    }

    #[test]
    fn claude_code_command_is_well_formed() {
        let cmd = claude_code_add_command(
            "vike-studio",
            &server_entry("/opt/vike-cli", Some("127.0.0.1:7878")),
        );
        assert!(cmd.starts_with("claude mcp add vike-studio"));
        assert!(cmd.contains("-- /opt/vike-cli mcp --addr 127.0.0.1:7878"));
    }
}
