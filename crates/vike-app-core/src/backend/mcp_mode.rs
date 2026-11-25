//! `vike-desktop --mcp`: the MCP server `vike-mcp` is, run by the desktop's own executable over stdio
//! with no window, so an agent host (Claude Desktop) launches the one program a user installed. The
//! desktop's `main` asks [`run`] first, before its settings, its log and its window, and an answer is
//! the process's exit.
//!
//! - **The same server.** [`run`] is `vike-mcp`'s own start and loop (`vike_mcp::boot`,
//!   `vike_mcp::run`), and the words after `--mcp` (`--node`, `--addr`, `--profile` …) are its flags,
//!   read by its parser: the same tools, answers and refusals. `crates/vike-desktop/tests/mcp_mode.rs`
//!   replays `vike-mcp`'s goldens against the built binary. Stdout is the protocol; every other line
//!   goes to stderr, which a client keeps in its log.
//! - **The desktop's settings, wherever the client starts it.** An agent host sets no working
//!   directory (Claude Desktop starts its servers in its own folder) and passes only a short list of
//!   variables, so the project walk starts at the folder this executable sits in: where the desktop,
//!   started from its folder, finds its settings and makes them on its first connect.
//!   `$VIKE_SETTINGS_DIR` still names the folder outright (the "Connect Claude" entry carries it). From
//!   there the device key and the dial rows (`config.node_addr`, `config.datahub_addr`) are the ones the
//!   desktop's Server group filed (`crate::backend::server_link`'s `connect_now`, over
//!   `vike_client::attach`).
//! - **The desktop's tunnel.** When that connect recorded a tunnel and `config.node_addr` is its silent
//!   mouth, it is raised again by the window's own start-up check (`server_link`'s `tunnel_to_raise`),
//!   on a thread of its own so the server answers at once: a node tool called before the forward is up
//!   fails as against a dead tunnel, and a later call reaches the node. ⚠ The tunnel OUTLIVES this
//!   process, as it outlives the window: it is this PC's one forward, which the window, this mode and
//!   `vike-cli` share and `vike-cli backend disconnect` closes, and a mouth that answers is never raised
//!   a second time. On Windows ssh is no child of this process at all (`vike_client::attach`'s module
//!   doc), and a launcher left behind by a kill mid-raise stops its ssh at `TUNNEL_WAIT`.
//!
//! ⚠ On Windows the release build is a GUI program (`windows_subsystem = "windows"`), and its pipes
//! still work: a client passes them in `STARTUPINFO` (`STARTF_USESTDHANDLES`), which a GUI program gets
//! like a console one, and std reads and writes a pipe handle as a file. Only a person typing into a
//! console reaches nothing, because a GUI program is given no console handles. No console call is made
//! here: one would open a window.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;

use vike_client::attach::{Words, port_answers};

use crate::backend::server_link::{BackendLink, connect_now, recorded_tunnel, tunnel_to_raise};

/// The first argument that makes the desktop the MCP server (and the one the Connect Claude entry passes).
pub(crate) const MCP_FLAG: &str = "--mcp";

/// `vike-desktop --mcp [FLAGS]`: `None` when `--mcp` is not the first argument (the window opens),
/// else the server's exit. `args` is the process's argv (argv[0] the program), `vars` its one
/// environment sweep.
#[must_use]
pub fn run(
    args: impl IntoIterator<Item = String>,
    vars: HashMap<String, String>,
) -> Option<ExitCode> {
    let flags = mcp_flags(args)?;
    let exe = std::env::current_exe().ok();
    Some(match vike_mcp::boot(vars, exe.as_deref().and_then(Path::parent)) {
        Ok(resolved) => {
            raise_recorded_tunnel(resolved.settings_dir.as_deref(), resolved.node_addr.as_deref());
            vike_mcp::run(flags, &resolved)
        }
        Err(code) => code,
    })
}

/// The words after `--mcp` when it is the FIRST argument (argv[0] is the program), else `None`. PURE.
/// The first word only, as `crate::ui::desktop_entry`'s `run` reads its own: a GUI's argv may carry
/// what a launcher appended, and the window opens on anything else (decision 0155).
pub(crate) fn mcp_flags(
    args: impl IntoIterator<Item = String>,
) -> Option<impl Iterator<Item = String>> {
    let mut args = args.into_iter().skip(1).peekable();
    let first = args.next_if(|first| first == MCP_FLAG);
    first.map(|_| args)
}

/// Raise the tunnel the desktop's connect recorded when `node_addr` dials its mouth and nothing
/// answers there (the module doc's third point): on a thread of its own, every word on stderr, and no
/// row rewritten (a connect with no process, as the window's start-up tunnel is).
fn raise_recorded_tunnel(settings_dir: Option<&Path>, node_addr: Option<&str>) {
    let (Some(dir), Some(dial)) = (settings_dir, node_addr) else { return };
    let Some(host) = tunnel_to_raise(recorded_tunnel(dir), dial, port_answers) else { return };
    let dir = dir.to_path_buf();
    let raising = std::thread::Builder::new().name("mcp-tunnel".into()).spawn(move || {
        let say = |line: &str| eprintln!("vike-desktop {MCP_FLAG}: {line}");
        let words = Words { say: &say, warn: &say, inherit: false };
        say(&format!("the recorded tunnel's mouth is silent: raising it again to {host}"));
        if let Err(e) = connect_now(&BackendLink::Tunnel(host.clone()), &dir, None, &words) {
            say(&format!("could not raise the tunnel to {host}: {e}"));
        }
    });
    if let Err(e) = raising {
        eprintln!("vike-desktop {MCP_FLAG}: could not start the tunnel's thread: {e}");
    }
}

#[path = "mcp_mode_tests.rs"]
#[cfg(test)]
mod mcp_mode_tests;
