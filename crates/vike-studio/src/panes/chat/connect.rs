//! Wire the user's own local Claude to the vike MCP server — the StudioChatPane "Connect to Claude"
//! helper behind the non-default `mcp` feature. Successor of the retired `vike-mcp` crate's
//! `connect` module (Phase B of the two-MCP-server consolidation, #842): the spawned server is now
//! the `vike-mcp` program, whose run/list tools go over a RUNNING `vike-datahub`
//! server (`--addr`, default `127.0.0.1:7878`) instead of an in-process DataFusion store — so the
//! generated command carries the datahub address, never a `VIKE_HIST_STORE` path. Pure +
//! unit-tested: build the server entry and emit the `claude mcp add …` line for Claude Code. (The
//! old module's Claude-Desktop config-JSON merge helper was dropped with the crate — the Studio
//! only ever surfaced the copy-pasteable command.)

/// The pieces of a `claude mcp add` invocation: the executable and its argv tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StudioChatMcpServerEntry {
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
}

/// Build the entry for the `vike-mcp` program. `datahub_addr`, when given, is
/// passed as `--addr` (the vike-datahub server the run/list tools dial — the Studio threads its
/// Source backend's address here); absent = the spawned server falls back to its own default
/// (`127.0.0.1:7878`, the SSH-tunnelled port of the deploy runbook). The spawned server signs
/// with this box's own device key from its settings database (decision 0288); no key is ever
/// written into a generated command.
pub(crate) fn server_entry(exe: &str, datahub_address: Option<&str>) -> StudioChatMcpServerEntry {
    let mut args = Vec::new();
    if let Some(address) = datahub_address {
        args.push("--addr".to_string());
        args.push(address.to_string());
    }
    StudioChatMcpServerEntry { command: exe.to_string(), args }
}

/// The copy-pasteable `claude mcp add …` command for Claude Code.
pub(crate) fn claude_code_add_command(name: &str, entry: &StudioChatMcpServerEntry) -> String {
    let mut parts = vec![format!("claude mcp add {name}"), "--".to_string()];
    parts.push(entry.command.clone());
    parts.extend(entry.args.iter().cloned());
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_entry_spawns_the_program_with_no_arguments() {
        let e = server_entry("/opt/vike-mcp", None);
        assert_eq!(e.command, "/opt/vike-mcp");
        assert!(e.args.is_empty(), "no --addr when no datahub address given");
    }

    #[test]
    fn server_entry_carries_the_datahub_address() {
        let e = server_entry("/opt/vike-mcp", Some("<host>:7878"));
        assert_eq!(e.args, vec!["--addr".to_string(), "<host>:7878".to_string()]);
    }

    #[test]
    fn claude_code_command_is_well_formed() {
        let command = claude_code_add_command(
            "vike-studio",
            &server_entry("/opt/vike-mcp", Some("127.0.0.1:17878")),
        );
        assert!(command.starts_with("claude mcp add vike-studio"));
        assert!(command.contains("-- /opt/vike-mcp --addr 127.0.0.1:17878"));
    }
}
