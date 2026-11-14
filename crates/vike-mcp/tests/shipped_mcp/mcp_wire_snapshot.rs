//! **Golden snapshots of the MCP server AT THE WIRE**: the shipped program, spawned, driven over its
//! real stdio, and every byte that came back compared against committed files. Nothing else pins the
//! bodies: `crates/vike-mcp/src/tests/` runs in process and asserts by `contains`, and
//! `crates/vike-mcp/tests/shipped_mcp/mcp_transcript.rs` asserts the gate over `serve`, not the program.
//! `scripts/cli_mcp_smoke.sh` runs in no workflow and `agent-eval.yml` only by hand.
//!
//! # Why it exists: step 0 of the CLI + MCP re-architecture
//!
//! The server moved to a program of its own and is going onto a protocol library. Both moves must
//! change no answer an agent can see. So these goldens were written against the OLD code first, and
//! the later code runs THE SAME FILES: [`LAUNCH`] and [`SELF_NAMES`] are the only two things here that
//! say which program this is.
//!
//! # Regenerating: one command
//!
//! ```sh
//! cargo test -p vike-mcp --test shipped_mcp -- --ignored bless_
//! ```
//!
//! Run it in a lane (nothing compiles on the dev box), then READ THE DIFF. The comparison exists to
//! make a change visible; a bless nobody read turns this suite into a rubber stamp. It wipes and
//! rewrites `tests/fixtures/mcp_wire/`. The CLI half is
//! `crates/vike-cli/tests/shipped_cli/cli_surface_snapshot.rs`, with its own command.
//!
//! # What a golden is
//!
//! One JSON document per session: the argv after [`LAUNCH`], the environment, every line in both
//! directions in order, the exit code and stderr. Compared as PARSED JSON, so a key's position is
//! not a difference and the order of an array IS (the tool roster's above all). A multi-line string
//! is stored as `{"<lines>": [..]}` so a person can read it, on both sides of the compare; a tool's
//! `content[0].text` that is exactly the pretty rendering of its `structuredContent` is stored as
//! [`harness::RENDERED`]. The `instructions` text is plain text, `instructions.<profile>.txt`,
//! compared byte for byte.
//!
//! # Normalised, and nothing else: only what really varies
//!
//! * the scratch directory (`<scratch>`), the node's and the fake daemon's ephemeral port;
//! * the name the program calls itself on stderr and in its usage ([`SELF_NAMES`], `<program>`);
//! * the minted `order_client_id` (random session prefix): `<minted-order-client-id-N>`, in order of appearance;
//! * `serverInfo.version` and a transcript's `proc.ver` (`<version>`), `--version`'s whole line;
//! * a transcript's `ts_ms`, `proc.pid`, `proc.bin` and its file's month;
//! * how old a read's frame is, `frame_age_ms` and its two siblings: left out, a clock reading on a node stood up per run.
//!
//! ⚠ **`preview_token` is NOT normalised**: it is `pv-N`, a per-process counter, so it does not vary,
//! and a token that started to is exactly what a golden should catch. The order client id a case
//! supplies itself (`c-…`) is not normalised either. Every dial goes to a fixed dead port
//! (`127.0.0.1:1`) unless a case stands a fake daemon up, so no real daemon on the box is ever
//! reached and a refused dial reads the same on every run.
//!
//! # Not pinned here, stays a unit test
//!
//! Panic guards (`__test_panic` is `cfg(test)`), the cancel races, account-epoch movement and a node
//! dropping. Preview EXPIRY is pinned here, at the price of one test that really waits past
//! `PREVIEW_WINDOW`: `gate::preview_token_expiry`.
//!
//! # A golden is no claim that an answer is right
//!
//! It pins what the server SAYS. Changing an answer is its own commit, with its goldens re-blessed and
//! visible in review.

#[path = "mcp_wire_snapshot/gate.rs"]
mod gate;
#[path = "mcp_wire_snapshot/harness.rs"]
mod harness;
#[path = "mcp_wire_snapshot/process.rs"]
mod process;
#[path = "mcp_wire_snapshot/protocol.rs"]
mod protocol;
#[path = "mcp_wire_snapshot/surface.rs"]
mod surface;
#[path = "mcp_wire_snapshot/tool_calls.rs"]
mod tool_calls;
#[path = "mcp_wire_snapshot/trace.rs"]
mod trace;

/// THE ONE LAUNCH: the program and the argv head every session starts with.
const LAUNCH: harness::Launch =
    harness::Launch { program: env!("CARGO_BIN_EXE_vike-mcp"), head: &[] };

/// The word the program calls ITSELF on stderr and in its usage text, in the goldens as `<program>`.
/// Text ON THE WIRE is not scrubbed: a tool description naming `vike-cli config set` means that
/// command, and `serverInfo.name` is literal.
const SELF_NAMES: &[(&str, &str)] = &[("vike-mcp", "<program>")];

/// One `#[test]` per case, and the list the bless walks: a case cannot be tested and not blessed.
macro_rules! golden_cases {
    ($($module:ident :: $case:ident),* $(,)?) => {
        $(#[test] fn $case() { harness::check($module::$case()); })*
        const ALL: &[fn() -> Vec<harness::Golden>] = &[$($module::$case),*];
    };
}

golden_cases! {
    surface::initialize,
    surface::tools_list,
    surface::resources,
    surface::prompts,
    tool_calls::offline_tools,
    tool_calls::tool_errors,
    tool_calls::withheld_tools,
    tool_calls::node_reads_without_a_node,
    tool_calls::node_reads_against_a_dead_node,
    tool_calls::dead_daemons,
    tool_calls::keyed_dials,
    tool_calls::keyed_hub_and_no_keys,
    gate::preview_gate_without_a_node,
    gate::preview_gate_with_a_paper_node,
    gate::preview_token_expiry,
    gate::unattended,
    protocol::protocol_errors,
    protocol::invalid_utf8_line,
    protocol::cancel_inline,
    protocol::cancel_deferred,
    protocol::id_in_flight_and_reused,
    protocol::eof_with_a_run_in_flight,
    protocol::last_backtest_resource,
    trace::trace_dir,
    trace::trace_project_state,
    trace::trace_of_a_cancelled_call,
    process::flags,
    process::boot,
}

/// THE WRITER (the module doc's command). Every case runs at once: the whole bless takes as long as
/// the one case that waits out the preview window.
#[test]
#[ignore = "writes into the source tree — run it deliberately, then read the diff"]
fn bless_mcp_wire_goldens() {
    let produced: Vec<Vec<harness::Golden>> = std::thread::scope(|scope| {
        let cases: Vec<_> = ALL.iter().map(|case| scope.spawn(*case)).collect();
        cases.into_iter().map(|c| c.join().expect("a case panicked")).collect()
    });
    harness::bless(produced.into_iter().flatten().collect());
}
