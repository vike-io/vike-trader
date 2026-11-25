//! The `vike-cli` subcommand modules. Each exposes a `run(args) -> ExitCode` entry the top-level
//! [`crate::dispatch`] routes to: [`backtest`] (compute-to-data offload), [`config`] (settings
//! provenance: every setting, its effective value, and WHERE that value came from —
//! settings-unification Phase 0), whose `check` verb lives one file over in [`config::check`]
//! because it answers a different question with a different product — an EXIT CODE a systemd
//! `ExecStartPre=` can refuse a unit with, rather than a disclosure — [`secrets`] (inspect the
//! credential store — the settings database `<project>/settings/db/vike.db`), [`init`] (scaffold `<project>/user_data` — the
//! user-content directory — with runnable examples in every folder) and [`indicators`] (print the
//! indicator functions a Rhai strategy can call, off the same const the host binds from — the
//! human twin of the MCP `list_indicators` tool). An interactive `repl` is the planned sibling
//! (add a module here + one arm in `dispatch`).
//! [`data`] is the store-filling, store-reading and store-EMPTYING plane — its verbs live in groups
//! (`data history fetch`, `data history ls`, `data history rm`, …; `fetch --source starter` is what
//! `fetch-starter` became, and `seed-demo` is gone with the synthetic tape) — whose local write half routes to the store tool
//! (`vike-backend datahub store`), so a person who installed "the vike CLI" does not have to learn a second
//! binary's name to get market data for the backtest they just scaffolded. Ruling 12 of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` is what moved the last
//! four of those off the engine's own flag surface.
//! [`trade::status`] is the read-only node question `vike-cli trade status` answers — the trading
//! MODE and the mounted-strategy registry in one output, over the split-plane B4 wire verb plus a
//! point-in-time snapshot, observe scope only. It was the top-level `strategy-status` until ruling
//! 17 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` folded it into the
//! `trade` family and deleted the REPL `state` verb that both read and wrote.
//! [`report`] and [`research::study`] were ruling 16's two OWED client halves — `vike-cli <X>` asks the
//! backend to do X, and both operations had a backend verb with nothing on this side. `report`
//! asked a vike-tradehub for a tearsheet over the journal IT writes, until that door was RETIRED on
//! 2026-11-18 (`report --node` is refused at the parse); a live account's report is
//! `report account:<venue>/<LABEL>`, its fills read from the datahub ([`vike_client::account_report`]), and the
//! source that asks no backend at all is `report run:<run>`, which renders a FINISHED run from its
//! own directory: [`report::stored`]. That half exists because the
//! verb named `report` could not report on a backtest this machine had already computed — a hole
//! no server arm closes, since nothing crosses a wire to re-render a local directory;
//! [`report::schema`] is the published shape of what the stored half emits — a version on the
//! document and a JSON Schema `report --schema` prints with no run, no node and no key;
//! `vike-cli research study` asks the backtesthub to run a study FILE next to the store IT holds.
//! ⚠ **Its server arm has LANDED**: `Request::RunStudy` has been served since stage 7 —
//! MOUNT-conditionally, so a peer that mounted no study runner still refuses by name. What survives
//! is the NEGOTIATION rather than the absence; `cmd::research::study`'s module doc argues its scope decision
//! where the code is, and `crates/vike-cli/CLAUDE.md` carries which refusal an operator can still
//! meet and on which box.
//! [`research`] is the plane BEFORE a strategy exists — investigate a signal, fit a model — and
//! [`research::study`] is its one sub-verb today (ruling 1 of the 2026-09-13 owner rulings). It routes and
//! nothing else: the grammar, the address ladder, the negotiation and every message stay in
//! [`research::study`], where the scope decision is argued beside the code.
//! [`args`] is the shared hand-rolled flag-parsing glue every command's own parser drains;
//! [`engine`] is the ONE place the standalone `backtesthub` binary is located and spawned — the
//! `--local` half of [`backtest`] and the whole of the `data` verb go through it, because
//! this crate may not LINK that engine (its identity is DataFusion-free) and three copies of
//! "where is it, and what does its exit code mean" would drift;
//! [`vike_client::verbs`] is the ONE node-write verb vocabulary (args→`CommandWire` construction + the order client id
//! mint + the client-side guardrail) shared by the `trade` REPL and `vike-mcp`'s write tools — orders
//! AND the three node-lifecycle verbs, which each surface built for itself until both grew a
//! spelling for them, and
//! [`vike_client::hubkeys`] is the ONE keyring both of those surfaces authenticate with (this box's
//! device key, decision 0288 item 8, and nowhere else);
//! [`backend`] is the ONBOARDING surface for the daemon that key authenticates to — `mint-missing`
//! makes the server box's own keys (it never accepts one and never prints one), and
//! `connect`/`status`/`disconnect` are the client's half on a laptop. It is the second credential
//! WRITER in this crate, and it is `secrets set`'s shape with the value's source removed entirely:
//! there is no operator-supplied key at all.
//! ⚠ Its `ping` sub-verb belongs to that family by ADDRESS rather than by service, and its own
//! module doc opens with the wart: it dials the **vike-datahub** protocol (the data server and the
//! backtesthub both) and prints what the handshake advertises — the protocol version, the
//! capability set verbatim, whether a Studio runner table is mounted and the auth posture — where
//! every sibling verb is about a `vike-tradehub` node. It configures nothing and writes nothing,
//! which is what admits it here; `status` is not the same question with an address.
//! ⚠ [`backtest::render::walkforward`] is NOT in that list and is no longer a command at all: decision 3 of
//! `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` folded the verb into
//! `backtest run`, where the PROFILE's own `[walkforward]` table selects the walk. What is left in
//! that module is the REPORT RENDERER for the server's `WalkforwardResults`, `pub(crate)` and called
//! by [`backtest`] alone — it exposes no `run` and the dispatcher routes to nothing in it. Its
//! file sits under `backtest/render/`, beside the renderer that calls it.
//!
//! //! [`backtest::read`] is the READING half of the backtest plane — `backtest ls|show|path` over
//! `<project>/user_data/runs/`, artifact-only: no engine, no store, no socket. [`backtest::params`] answers
//! "what knobs exist" for a Rhai script or a built-in strategy, offline, and is where
//! `backtest --list-params` was re-homed to; [`backtest::strategies`] holds the plane's STRATEGY-AUTHORING
//! sub-verbs — `strategies` itself, the one reading verb that DIALS the backtesthub (because the
//! roster it prints is the SERVER's rather than this build's), plus the three that answer about a
//! strategy before there is a run to read: `templates` (the starters this binary ships, listed,
//! printed or saved to a new file), `script-api` (everything an authored Rhai script may call,
//! derived from the host's own registration) and `script-check` (compile one offline, and put the
//! verdict in the exit code). The last three exist because a RELEASE install has no source tree —
//! `indicators`' argument, applied to the rest of the same surface — and because the MCP tools had
//! shipped two of those answers to an agent for months while a human with the same binary had
//! neither.

/// PARSE ONLY: would this binary's real parser accept `vike-cli <argv…>`? For every place this
/// crate PRINTS a command for a person to type — `crate::cmd::init`'s next steps first — so that a
/// printed line is held to the grammar it names rather than to a second copy of its spelling.
///
/// ⚠ Wired for the verbs something here prints, and REFUSING every other one by name. A checker
/// that answered `Ok` for a verb whose parser it never ran would be the exact failure it exists to
/// catch — a printed line nothing checked — so a new verb in a printed line reddens the test until
/// its parser is wired in here.
///
/// ⚠ It stops at the PARSE. What a verb then does at run time — dial a server, spawn the engine,
/// open a store — is not asked, so an `Ok` means "no usage error", never "this will work on a
/// fresh install". That second question is the printed prose's to answer honestly.
#[cfg(test)]
pub(crate) fn accepts(argv: &[&str]) -> Result<(), String> {
    match argv.split_first() {
        Some((&"data", rest)) => data::accepts(rest),
        Some((&"backtest", rest)) => backtest::accepts(rest),
        Some((&"secrets", rest)) => secrets::accepts(rest),
        Some((&"account", rest)) => secrets::account_accepts(rest),
        Some((&"access", rest)) => access::accepts(rest),
        Some((&"backend", rest)) => backend::accepts(rest),
        Some((verb, _)) => Err(format!(
            "`vike-cli {verb}` has no parse-only entry in `crate::cmd::accepts` — wire its parser \
             there before printing a `{verb}` line"
        )),
        None => Err("an empty command line".to_string()),
    }
}

pub mod access;
pub(crate) mod args;
pub mod backtest;
pub(crate) mod completions;
pub mod config;

pub mod backend;
pub mod data;
pub(crate) mod engine;
pub mod gatewayhub;
pub mod indicators;
pub mod init;
pub mod report;
pub mod research;
pub mod secrets;
pub(crate) mod settings_write;
pub(crate) mod strategy;
pub mod surface;
pub mod trade;

/// Commands ANOTHER crate prints for an operator to type into `vike-cli`, held to this parser from
/// the one crate that can run it.
///
/// ⚠ The engine side is reached through the `vike-backtest` DEV-dependency, which is why this
/// lives under `#[cfg(test)]` and nowhere else: the library links no engine crate (its identity is
/// DataFusion-free), and a test build is the one configuration where both halves are in scope.
#[cfg(test)]
mod printed_elsewhere {
    use vike_backtest::run::data_plan::{CoverageFinding, CoverageVerdict};
    use vike_data::{SeriesId, SeriesMissingSpan, SeriesShortfall};
    use vike_model::StoreKindId;

    /// The ENGINE's `fill it with:` line under a coverage refusal is a `vike-cli` command, and it
    /// printed the flat `vike-cli data fetch …` after this binary had started refusing that
    /// spelling (#2072 moved every `data` verb under `history`). vike-backtest cannot link vike-cli,
    /// so its own test pins only a prefix; this drives the whole rendered line through the parser.
    #[test]
    fn the_engines_fill_hint_is_a_command_vike_cli_accepts() {
        let verdict = CoverageVerdict {
            findings: vec![CoverageFinding {
                id: SeriesId::per_symbol(
                    StoreKindId::Bar,
                    "binance",
                    "BTCUSDT",
                    Some("1h".to_string()),
                ),
                spans: vec![SeriesMissingSpan {
                    start_ms: 1_735_689_600_000,
                    end_ms: 1_738_368_000_000,
                    kind: SeriesShortfall::Leading,
                }],
                absent: false,
            }],
            unprovable: false,
        };
        let lines = verdict.lines();
        let command = lines
            .iter()
            .find_map(|l| l.trim().strip_prefix("fill it with: "))
            .unwrap_or_else(|| panic!("a per-symbol bar hole offered no fill command: {lines:?}"));
        let argv: Vec<&str> = command.split_whitespace().collect();
        assert_eq!(
            argv.first(),
            Some(&"vike-cli"),
            "the fill hint names another program: {command}"
        );
        super::accepts(&argv[1..]).unwrap_or_else(|e| {
            panic!("the engine tells an operator to run `{command}`, which vike-cli refuses: {e}")
        });
    }
}

#[cfg(test)]
mod hub_dial_tests;
