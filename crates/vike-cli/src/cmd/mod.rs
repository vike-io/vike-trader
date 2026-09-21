//! The `vike-cli` subcommand modules. Each exposes a `run(args) -> ExitCode` entry the top-level
//! [`crate::dispatch`] routes to: [`backtest`] (compute-to-data offload) and [`mcp`] (the stdio MCP
//! server exposing the create+backtest tools to an agent), [`config`] (settings
//! provenance: every setting, its effective value, and WHERE that value came from —
//! settings-unification Phase 0), whose `check` verb lives one file over in [`config_check`]
//! because it answers a different question with a different product — an EXIT CODE a systemd
//! `ExecStartPre=` can refuse a unit with, rather than a disclosure — [`secrets`] (inspect the
//! credential store — `<project>/settings/secrets.env`, or the settings database
//! `<project>/settings/db/vike.db` once `docs/decisions/0054`'s migration has run on this box, and
//! from then on that database answers WHOLLY), [`init`] (scaffold `<project>/user_data` — the
//! user-content directory — with runnable examples in every folder) and [`indicators`] (print the
//! indicator functions a Rhai strategy can call, off the same const the host binds from — the
//! human twin of the MCP `list_indicators` tool). An interactive `repl` is the planned sibling
//! (add a module here + one arm in `dispatch`).
//! [`data`] is the store-filling, store-reading and store-EMPTYING verb — `fetch`, `fetch-starter`,
//! `seed-demo`, `export`, `list`, `coverage` and `rm` — whose write half routes to the same
//! standalone engine, so a person who installed "the vike CLI" does not have to learn a second
//! binary's name to get market data for the backtest they just scaffolded. Ruling 12 of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` is what moved the last
//! four of those off the engine's own flag surface.
//! [`trade_status`] is the read-only node question `vike-cli trade status` answers — the trading
//! MODE and the mounted-strategy registry in one output, over the split-plane B4 wire verb plus a
//! point-in-time snapshot, observe scope only. It was the top-level `strategy-status` until ruling
//! 17 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` folded it into the
//! `trade` family and deleted the REPL `state` verb that both read and wrote.
//! [`report`] and [`study`] are ruling 16's two OWED client halves — `vike-cli <X>` asks the
//! backend to do X, and both operations had a backend verb with nothing on this side. `report`
//! asks a vike-tradehub node for a tearsheet over the journal IT writes (the reason it is the
//! stronger case: that journal lives on the daemon's box, so the answer used to require an SSH
//! session) — and it now has a SECOND SOURCE that asks no backend at all: a run SELECTOR renders a
//! FINISHED run from its own directory, which is [`report_stored`]. That half exists because the
//! verb named `report` could not report on a backtest this machine had already computed — a hole
//! no server arm closes, since nothing crosses a wire to re-render a local directory;
//! [`report_schema`] is the published shape of what the stored half emits — a version on the
//! document and a JSON Schema `report --schema` prints with no run, no node and no key;
//! `vike-cli research study` asks the backend to fit a compiled study next to the store IT holds.
//! ⚠ **Both those server arms have LANDED**, and this paragraph said the node half *"refuses on
//! every box until its server arm lands"* and that each verb ships *"as the CLIENT half of a
//! capability-negotiated verb whose server arm is a follow-up"*. Both were exact when written:
//! `crates/vike-tradehub/src/server.rs`'s `tearsheet_reply` now serves the tearsheet and that
//! file's `served_features` advertises the capability unconditionally, and `Request::RunStudy` has
//! been served since stage 7 — MOUNT-conditionally, so a peer that mounted no study runner still
//! refuses by name. What survives is the NEGOTIATION rather than the absence; each module doc
//! argues its own scope decision where the code is, and `crates/vike-cli/CLAUDE.md` carries which
//! refusal an operator can still meet and on which box.
//! [`research`] is the plane BEFORE a strategy exists — investigate a signal, fit a model — and
//! [`study`] is its one sub-verb today (ruling 1 of the 2026-09-13 owner rulings). It routes and
//! nothing else: the grammar, the address ladder, the negotiation and every message stay in
//! [`study`], where the scope decision is argued beside the code.
//! [`args`] is the shared hand-rolled flag-parsing glue every command's own parser drains;
//! [`engine`] is the ONE place the standalone `backtest` binary is located and spawned — the
//! `--local` half of [`backtest`] and the whole of the `data` verb go through it, because
//! this crate may not LINK that engine (its identity is DataFusion-free) and three copies of
//! "where is it, and what does its exit code mean" would drift;
//! [`verbs`] is the ONE node-write verb vocabulary (args→`WireCommand` construction + the coid
//! mint + the client-side guardrail) shared by the `trade` REPL and the `mcp` write tools — orders
//! AND the three node-lifecycle verbs, which each surface built for itself until both grew a
//! spelling for them, and
//! [`nodekeys`] is the ONE resolver for the vike-tradehub node keys both of those surfaces
//! authenticate with (process env, then the credential store the daemon itself reads);
//! [`node`] is the ONBOARDING surface for the daemon those keys authenticate to — `setup` MINTS
//! both of them on the daemon's box (it never accepts one and never prints one), and
//! `connect`/`status`/`disconnect` are the client's half on a laptop. It is the second credential
//! WRITER in this crate, and it is `secrets set`'s shape with the value's source removed entirely:
//! there is no operator-supplied key at all.
//! ⚠ Its `ping` sub-verb belongs to that family by ADDRESS rather than by service, and its own
//! module doc opens with the wart: it dials the **vike-datahub** protocol (the data server and the
//! compute server both) and prints what the handshake advertises — the protocol version, the
//! capability set verbatim, whether a Studio runner table is mounted and the auth posture — where
//! every sibling verb is about a `vike-tradehub` node. It configures nothing and writes nothing,
//! which is what admits it here; `status` is not the same question with an address.
//! ⚠ [`walkforward`] is NOT in that list and is no longer a command at all: decision 3 of
//! `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` folded the verb into
//! `backtest run`, where the PROFILE's own `[walkforward]` table selects the walk. What is left in
//! that module is the REPORT RENDERER for the server's `WalkForwardReport`, `pub(crate)` and called
//! by [`backtest`] alone — it exposes no `run` and the dispatcher routes to nothing in it. The file
//! keeps its path deliberately; that module's own doc argues why.
//!
//! [`mcp_trace`] is the `mcp` server's own opt-in AGENT TRANSCRIPT — one appended JSONL record per
//! `tools/call`, arguments redacted — a sibling of `vike_model::change_journal` rather than a
//! record kind in it, for the reasons that module's "A future sibling" section states.
//!
//! [`runs`] is the READING half of the backtest plane — `backtest ls|show|path` over
//! `<project>/user_data/runs/`, artifact-only: no engine, no store, no socket. [`params`] answers
//! "what knobs exist" for a Rhai script or a built-in strategy, offline, and is where
//! `backtest --list-params` was re-homed to; [`strategies`] holds the plane's STRATEGY-AUTHORING
//! sub-verbs — `strategies` itself, the one reading verb that DIALS the compute daemon (because the
//! roster it prints is the SERVER's rather than this build's), plus the three that answer about a
//! strategy before there is a run to read: `templates` (the starters this binary ships, listed,
//! printed or saved to a new file), `script-api` (everything an authored Rhai script may call,
//! derived from the host's own registration) and `script-check` (compile one offline, and put the
//! verdict in the exit code). The last three exist because a RELEASE install has no source tree —
//! `indicators`' argument, applied to the rest of the same surface — and because the MCP tools had
//! shipped two of those answers to an agent for months while a human with the same binary had
//! neither.

pub(crate) mod args;
pub mod backtest;
pub mod config;
pub mod config_adopt;
pub mod config_check;

pub mod config_mirror;
pub mod config_mirror_recorder;
pub mod config_recorder;
pub mod config_set;
pub mod data;
pub mod datahub;
pub(crate) mod engine;
pub mod indicators;
pub mod init;
pub mod mcp;
pub(crate) mod mcp_trace;
pub mod node;
pub mod nodekeys;
pub(crate) mod params;
pub mod report;
pub(crate) mod report_schema;
pub(crate) mod report_stored;
pub mod research;
pub(crate) mod runs;
pub mod secrets;
pub mod secrets_move;
pub(crate) mod settings_write;
pub(crate) mod strategies;
pub mod study;
pub mod surface;
pub mod trade;
pub mod trade_status;
pub mod verbs;
pub(crate) mod walkforward;
