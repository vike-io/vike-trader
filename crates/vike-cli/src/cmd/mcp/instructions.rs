//! The `initialize` instructions: one clause per capability, scoped by the session's tool access.

use super::WRITE_TOOLS;
use super::node_writes::LIFECYCLE_TOOLS;
use super::scope::ToolAccess;
#[cfg(doc)]
use super::{Server, node_writes::unattended_refusal};

/// The OPENING line of [`instructions`] — what this server IS, and the one fact that makes the rest
/// of the text worth its context window: it is a PART.
///
/// ⚠ It ended *…of a larger system, and that is the thing no tool description can tell you* until
/// the backtest bullet in [`INSTRUCTIONS_ELSEWHERE`] had to name a datahub, and that clause is where
/// most of the bytes came from — it argued for the fact rather than stating one, and the
/// ELSEWHERE's own first line (*NAME THE COMMAND rather than refusing blind*) is what an agent acts
/// on. The fact itself, that this is a PART, is untouched.
const INSTRUCTIONS_OPENING: &str = "\
This is `vike-cli`'s agent surface: author and validate Rhai strategies, run backtests, and read \
and control one running vike-tradehub node. It is ONE surface of a larger system.";

/// The ELSEWHERE — the operator-side commands that do what no tool here does.
///
/// ⚠ Every command named here is checked by `the_instructions_name_only_real_commands` against the
/// module that OWNS it (`crate::COMMANDS` for a verb, that command's own `USAGE` for a subcommand
/// or a flag), because a surface that names a command the binary does not have is worse than one
/// that names none: it sends an operator to a terminal to type something that fails.
/// ⚠ **Every word here is paid for by `the_instructions_stay_short_enough_to_prepend_to_every_session`'s
/// ceiling, which this text now shares with a fifth clause.** It was compressed when
/// [`INSTRUCTIONS_DELETE`] landed — the facts and every gated command spelling are unchanged, the
/// connective tissue is not. Adding a sentence here means taking one from somewhere.
///
/// ⚠ **The credential bullet keeps `settings/secrets.env` and calls the other store `a DB`, and
/// every word of that was decided by measurement rather than taste.** It used to end *One store:
/// `<project>/settings/secrets.env`*, which `docs/decisions/0054`'s credential half turned into a
/// claim about the wrong artifact on a migrated box. Three constraints then closed on the
/// replacement, each measured on the CI box after the previous one was satisfied:
///
/// 1. **The ceiling.** Spelling both stores in full cost 78 bytes and put the joined text at
///    **2067** against `mcp_instructions_gate.rs`'s 2000.
/// 2. **The eval case.** Dropping the paths for *`secrets path` names it* then failed
///    `the_four_elsewhere_cases_are_satisfiable_by_an_agent_that_read_the_instructions`:
///    `vike_agent_eval::cases`' `READ_THE_CREDENTIAL_STORE` grades an answer on naming a store, so
///    a surface carrying none would have left that check grading the model's memory of this
///    product.
/// 3. **The two tests in this module.** `initialize_carries_instructions_that_name_the_surface_beyond_this_one`
///    and `the_instructions_are_scoped_by_the_same_access_the_roster_is` both demand the literal
///    `settings/secrets.env` — so the bare basename that satisfied (1) and (2) still failed, and
///    the DIRECTORY-qualified spelling is not optional here.
///
/// Everything below `settings/secrets.env` therefore had to go: 42 bytes, which leaves 4 under the
/// ceiling. The database is named only as a KIND, and that is the honest minimum — it says the file
/// may not be what answers, and `vike-cli secrets path`, two words earlier in the same bullet, says
/// which does. The path, the per-RUN choice and the shadowing live in `skills/`, which an operator
/// installs deliberately and which no session pays for.
///
/// ⚠ **The backtest bullet read *backtest with no server: `vike-cli backtest run --local …`* until
/// decision 0084's 2026-09-25 amendment made it false**, and it was false in the way that costs an
/// operator most: the answer to "can I backtest without a server" was a flat yes. `--local` moves
/// the ENGINE, not the history — the spawned engine reads through a datahub like every other
/// reader, and `--store DIR` is refused — so on a box with files and no server the command is a
/// datahub started on them beside `--local`. That is `VIKE_DATAHUB_STORE=DIR vike-backend datahub`:
/// with no node keys it authenticates nothing and binds loopback only, which is where the engine
/// dials by default. The span starts with the environment assignment, so
/// `crates/vike-ops/tests/docs/mcp_instructions_gate.rs`'s binary check skips that assignment to reach
/// `vike-backend`, and `vike_agent_eval::cases`' `RUN_A_BACKTEST_LOCALLY` now grades an answer on
/// naming it.
///
/// Naming it put the joined text at **2063** against the ceiling (the gate's own parse). The bytes
/// came from two places that carried no fact: the record bullet's *— part of the data daemon now*
/// (a note from the day ruling 10 merged the recorder in, which the command itself now says) and
/// the opening's closing clause (see [`INSTRUCTIONS_OPENING`]). `key-less` did not fit, and it is
/// not lost: the refusal an operator meets on `--store`,
/// `vike_datahub_client::flag_vocab::store_flag_removed`, says it in the same breath as the command.
const INSTRUCTIONS_ELSEWHERE: &str = "\
When a request needs another part, NAME THE COMMAND rather than refusing blind — none is a tool \
you can call; a human runs them:
- record a live tape: `vike-backend datahub --record <profile>`.
- get bars in: `vike-cli data hist fetch binance:BTCUSDT:1h --days 180` (needs a datahub), or \
`vike-cli data hist fetch --source demo` for a synthetic tape.
- backtest on this box: `vike-cli backtest run --local --profile run.toml`. It still reads a \
datahub; for files here: `VIKE_DATAHUB_STORE=DIR vike-backend datahub`.
- which venue API keys this box holds and where from: `vike-cli secrets list` / \
`vike-cli secrets path`. One store: `settings/secrets.env` or a DB.";

/// The clause the credential bullet must keep, and it is its own paragraph rather than a tail on
/// that bullet because it qualifies the whole surface.
///
/// ⚠ An OMISSION is what an agent fills in with a guess: a store's path with no such sentence
/// beside it reads as an invitation to go and use it, and this text is read by a model that may
/// hold a shell this server knows nothing about.
/// `the_mcp_surface_advertises_no_credential_writer` holds the text to it — see
/// `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md`, which
/// records why the READ verbs above may be named here and the writer may not.
pub(super) const INSTRUCTIONS_NO_CREDENTIAL: &str = "\
No tool on this server reads or writes a credential, and none can arm a venue.";

/// The clause the recorder and data bullets earn only when the datahub reads are SERVED — under
/// `offline` there is no `list_series`, and pointing an agent at a tool this session does not have
/// is the same defect as pointing it at a command that does not exist.
const INSTRUCTIONS_SERIES: &str = "\
A tape or a fetch lands in the history store, and `list_series` is how you confirm it arrived.";

/// The WRITE clause, added only when a write tool is served. Under `read-only` and `offline` there
/// is no write tool, and a preview gate described to an agent that cannot reach it is noise.
///
/// ⚠ The SECOND sentence is here because the same run that forced this text also placed a real
/// order under an invented venue (`venue: "node"`), and a roster of tools cannot say where a venue
/// name comes from — `submit_order`'s schema says `venue` is a required string and nothing says a
/// string is not enough. The gate now refuses it ([`Server::vet_commanded_venue`]); this is the
/// half that stops an agent spending a turn discovering that.
const INSTRUCTIONS_WRITES: &str = "\
Writes are PREVIEW-GATED: without the `preview_token` this server issued for that exact command, \
a call returns a preview and sends nothing. A `venue` must be one the node MOUNTS — read it from \
`node_snapshot`'s `venues[].venue`, never infer it.";

/// The NODE-READ clause, added when the per-call node reads are served — every ring but `offline`.
///
/// ⚠ It exists because the three node reads look interchangeable in a tool list and are not, and
/// the failure that shape produces is a WRONG ANSWER rather than a refusal: an agent asked why a
/// strategy is not trading reaches for `node_snapshot`, finds a book with no orders in it and
/// nothing anywhere about mounts, and reports the node idle. Nothing in a per-tool description can
/// say "the question you are asking belongs to a different tool on this list".
///
/// ⚠ **It is two lines long because the length is GATED** —
/// `crates/vike-ops/tests/docs/mcp_instructions_gate.rs`'s
/// `the_instructions_stay_short_enough_to_prepend_to_every_session` caps the union of these
/// literals, which every client prepends to every session. So what each read RETURNS stays in its
/// own tool description, where a client shows it beside the tool; the only thing that cannot live
/// there, and therefore the only thing here, is the CONTRAST.
const INSTRUCTIONS_NODE_READ: &str = "\
`node_snapshot` is the BOOK, `strategy_status` is WHAT IS MOUNTED, `settings_show` is WHAT IT WAS \
CONFIGURED WITH — the wrong one reads as an empty answer, not as a wrong tool.";

/// The LIFECYCLE clause, added when a [`LIFECYCLE_TOOLS`] member is served. Under `read-only` and
/// `offline` there is none, and describing a gate an agent cannot reach is noise.
///
/// ⚠ The last sentence is the OWNER's rule (`docs/decisions/0086` point 6), and it is an
/// instruction rather than a gate because nothing on this side can see the chat it is about: a
/// settings write changes a live setting on the node, so an agent makes one only after the owner
/// said yes. It REPLACED a sentence that told an agent to obtain the operator's RETYPING of the key
/// as `policy_confirm` — an argument deleted with the retype itself (point 7). Where a session HAS
/// no chat, the rule cannot be kept by asking, and `--unattended` holds it structurally instead
/// ([`unattended_refusal`]).
pub(super) const INSTRUCTIONS_LIFECYCLE: &str = "\
`mount_strategy` / `unmount_strategy` / `set_setting` change the node's CONFIGURATION, not its \
book. A `set_setting` changes a LIVE setting on the node: make one only after the owner said yes \
in chat.";

/// The DELETE clause, added only when `delete_series` is served.
///
/// ⚠ **This clause REPLACES a proposed exclusion, and the reversal is why it says what it says.**
/// The original design forbade the instructions from naming any destroying subcommand at all, on
/// the argument that a model handing an operator the command has caused the deletion at one remove.
/// The owner lifted the exclusion on 2026-09-07: an agent that CAN call the tool must be told how it
/// works, and telling it less does not make the tool safer — it makes an under-briefed model likelier
/// to call it wrongly.
///
/// Three facts, and each is one a tool description cannot carry alone: that the grant is
/// irreversible in a way an order is not, that this surface asks MORE of a delete than the CLI does,
/// and that the TOKEN is the gate rather than `confirm` — the last of which
/// `every_write_touching_prompt_teaches_the_token_not_just_confirm` will fail a text that omits.
const INSTRUCTIONS_DELETE: &str = "\
`delete_series` is IRREVERSIBLE and its window may not be re-fetchable. It needs `produced_by` on \
EVERY call (more than the CLI's `rm` asks), and executes only on a second call carrying `confirm` \
AND that plan's `preview_token`.";

/// The CLOSING line — the two failure modes this text exists to prevent, stated as rules.
const INSTRUCTIONS_CLOSING: &str = "\
Never invent a tool name, and never report an operator-side command as something you ran.";

/// The MCP `instructions` field: free text a client shows the model as guidance about this server.
///
/// # Why it exists at all
///
/// A tool description can only describe ITS tool. Nothing in a roster of tools can say *what this
/// server is not*, and the measurement that forced this said so plainly: in the 2026-09-06
/// model-in-the-loop run (`.github/workflows/agent-eval.yml`, driver `claude-cli`) the agent
/// answered four cases WELL — it reported the datahub unreachable, enumerated what its toolset
/// covers, and told the operator the missing capability was a separate step — and failed all four
/// on the same shape: it never named the binary that does the thing, because nothing it could see
/// says that binary exists. An agent that must refuse and can also DIRECT is strictly more useful
/// than one that refuses blind, and this is the only channel in the protocol that carries it.
///
/// # Why it is this short
///
/// It is prepended to every session's context, so it is guidance and not a manual. It therefore
/// carries exactly what a tool description CANNOT: the fact that this is one part of a system, one
/// line per capability that lives outside it, and two rules about answering. It describes no tool
/// (the roster already does, better), teaches no procedure (`skills/` does, and those are rendered
/// from the code by `scripts/gen_skills.sh`), and repeats no argument name. Everything here is a
/// fact an agent cannot obtain from `tools/list` at any length.
///
/// # Scoping
///
/// ⚠ Scoped by the SAME [`ToolAccess`] the roster and the router are — telling a `read-only` agent
/// about a preview gate it cannot reach, or an `offline` one about a series list it does not have,
/// is the identical defect as advertising a withheld tool. The operator-side commands are NOT
/// scoped and must not be: they are things a HUMAN runs, and no profile of this server changes what
/// the operator can type.
pub(super) fn instructions(access: &ToolAccess) -> String {
    let mut parts = vec![INSTRUCTIONS_OPENING, INSTRUCTIONS_ELSEWHERE, INSTRUCTIONS_NO_CREDENTIAL];
    if access.admits("list_series") {
        parts.push(INSTRUCTIONS_SERIES);
    }
    // The per-call node reads, keyed on one of the two: they share a ring (both declare
    // `openWorldHint`, neither is a write), so either name answers for the clause.
    if access.admits("strategy_status") {
        parts.push(INSTRUCTIONS_NODE_READ);
    }
    if WRITE_TOOLS.iter().any(|t| access.admits(t)) {
        parts.push(INSTRUCTIONS_WRITES);
    }
    if LIFECYCLE_TOOLS.iter().any(|t| access.admits(t)) {
        parts.push(INSTRUCTIONS_LIFECYCLE);
    }
    if access.admits("delete_series") {
        parts.push(INSTRUCTIONS_DELETE);
    }
    parts.push(INSTRUCTIONS_CLOSING);
    parts.join("\n\n")
}
