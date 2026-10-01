//! `vike-cli backtest` — the thin REMOTE backtest command (headless two-layer plan, Layer 1, PR-1).
//!
//! Run a real backtest against a remote `vike-datahub` server (typically the CI box, next to the
//! 1.17B-row tape) from a laptop that has NO DataFusion in its build graph. This is the
//! compute-to-data proof-of-value: ship a small profile (its TOML text), get back the compact
//! report — the heavy history never crosses the wire. Built on ONLY what #719/#725 shipped
//! ([`DatahubClient::run_backtest`] over the existing wire proto): no new protocol, no new
//! dependency.
//!
//! # ⚠ `backtest` is a PLANE, and a sub-verb is ALWAYS required
//!
//! There is no bare `vike-cli backtest …` — decision 11 of
//! `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`, and it exists so that one
//! action has one spelling. A missing sub-verb prints [`USAGE`] and exits `2`, the way
//! `crate::cmd::data` already behaves. `--list-params` was a MODE FLAG here until that ruling and
//! is now `backtest params`; the old spelling answers with the new one rather than as an unknown
//! argument ([`LIST_PARAMS_RETIRED`]).
//!
//! # Usage
//!
//! ```text
//! vike-cli backtest run --venue binance --symbol BTCUSDT --interval 1d \
//!                       --from 2026-01-01T00 --to 2026-02-01T00 \
//!                       --strategy buy_hold --cash 10000 --fee 0.001
//! vike-cli backtest run --profile <run.toml> --set engine.slippage=0.0005
//! vike-cli backtest params --script <s.rhai>
//! ```
//!
//! - **`--profile <path>` is OPTIONAL** since stage 2 of the backtest-CLI-surface design. Flags
//!   BUILD a profile; `--profile` supplies a BASE the flags then override. Both routes converge on
//!   one text, which is what crosses the wire. A command line carrying neither a file nor anything
//!   to build one from is refused rather than shipping an empty document. The resulting text is
//!   shipped verbatim; the SERVER parses+validates it with `BacktestProfile::from_toml_str`.
//! - `--set <table>.<key>=<value>`, repeatable: the universal channel. Any key the profile schema
//!   declares. ⚠ Only the FIRST segment is checked here — a full schema check is impossible from
//!   this crate (it links no engine crate), so an undeclared `engine.*` key is refused on the far
//!   side, on exit rung 1. ⚠ `strategy.params.*` and `sweep.*` accept ANY key SILENTLY: their serde
//!   types are untyped, so a typo there is a genuine no-op nothing can catch.
//! - `--write-profile <path>` writes the built profile (refusing an existing path) and then runs;
//!   `--show-effective` prints it with each override's origin and stops, exit 0, dialling nothing.
//! - ⚠ `--align` from the design's §5.2 is NOT implemented: `DataCfg` declares no `align` field and
//!   no forward-fill or intersection code exists. A ragged multi-symbol universe is a named
//!   `HarnessError::Data` (stage 0's `refuse_ragged_series`), which is `strict` and nothing else.
//! - `--preset <path>`: a preset `.toml` — a flat table of a strategy's knobs, merged into the
//!   shipped profile's `[strategy.params]` (see [`merge_preset_params`]).
//! - `--addr <host:port>` (default `127.0.0.1:7880`): the COMPUTE daemon's address — `vike-backend
//!   backtest --addr`, NOT the datahub, since ruling 7 split the served surface. It binds
//!   localhost only; reach a remote one over `ssh -L 7880:localhost:7880 the CI box` (see the runbook,
//!   `docs/ops/datahub-the CI box.md`).
//! - `--json`: print the report JSON verbatim (as the server emitted it). Without it, the JSON is
//!   pretty-printed for a human — or, for a parameter search, rendered as the ranked table.
//!
//! # ⚠ A parameter SEARCH is this verb too, and the PROFILE is what selects one (ruling 13)
//!
//! ```text
//! vike-cli backtest run --profile <sweep.toml> --rank-by sharpe [--addr …]
//! vike-cli backtest run --local --profile <sweep.toml> --optimizer tpe --trials 128 --seed 7
//! ```
//!
//! There is no `sweep` verb. It existed until ruling 13 of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`, which deleted it: the
//! word "optimize" lives in the FLAG (`--optimizer <method>`), a second verb would be a second
//! name for one operation, and *searching a parameter space IS backtesting* — it is backtesting
//! many times and ranking the results. `vike-cli sweep` now fails naming this command.
//!
//! **A profile with a non-empty `[paramscan]` table runs a search; everything else runs one
//! backtest.**
//! That predicate is `BacktestProfile::is_paramscan`, which the engine and the compute server BOTH
//! branch on, and [`declares_a_paramscan_grid`] is this side's presence check over the same key — so
//! all three answer the same question the same way. ⚠ It closes a divergence that predates the
//! merge and is worth stating: the remote arm used to call `run_backtest` unconditionally, whose
//! server-side runner ignores the grid and reports ONE point, while `--local` handed the same
//! profile to an engine that ran the whole grid. Same command line, two different computations,
//! nothing in the output saying which.
//!
//! ⚠ **`--local` and `--addr` run the same search, and stage 7 is what made that true.**
//! `--optimizer grid|euler|tpe|genetic`, `--euler-depth`, `--trials`, `--seed` and
//! `--rank-by multi` all work on both routes: `vike_datahub_client::proto`'s
//! `Request::RunParamscanProfile` carries a `search` selector, and both sides resolve it through the
//! one `vike_backtest::harness::search_select`, so a knob refused under the wrong method is refused
//! with the same sentence whichever route ran. The three method KNOBS are forwarded as the TOKENS
//! you typed — this side parses none of them, which is what keeps that sentence single.
//!
//! ⚠ **The two SELECTORS are the exception, and they are canonicalised rather than parsed.** They
//! name members of a declared roster, so `vike_datahub_client::flag_vocab`'s `accept_value`
//! answers whether the spelling is admissible AND hands back the member's own spelling — which is
//! what makes `--rank-by SHARPE` and `--optimizer TPE` work here at all. They were a local exit-2
//! on this side and legal, tested values on the engine's until that wiring landed; this paragraph
//! said "the TOKENS you typed" of all five, which was the accurate description of a defect.
//!
//! ⚠ **A daemon older than that capability is told so, and nothing is sent.** It would decode the
//! frame, DROP the selector, run the exhaustive grid and report success — the silent downgrade
//! defect #1750 ended when it retired `--search`. `DatahubClient::run_paramscan_profile` checks
//! `vike_datahub_client::FEATURE_SEARCH_METHOD` in the handshake's feature list and refuses
//! locally; the failure is on the ordinary failure rung, naming the capability.
//!
//! ⚠ This paragraph read *"What `--local` can do and `--addr` cannot"* and listed five LOCAL-ONLY
//! knobs. That was true of every release before stage 7 and is kept here beside its correction,
//! because a withdrawn claim that simply vanishes teaches nobody.
//!
//! ⚠ `--rank-by` names how to ORDER results, not what work to do, so a VALID value is IGNORED —
//! not an error — on a profile with no `[paramscan]` table. That is the engine's documented behaviour
//! and this side does not second-guess it; an INVALID one is refused here, before a spawn or a
//! round trip — with the ENGINE'S OWN SENTENCE, rendered from the engine's own roster, and only
//! for a value no CASE of which is a member.
//!
//! ⚠ **One guard was deliberately NOT carried over from `sweep`.** That verb refused a profile
//! with no `[paramscan]` table before spawning, because it PROMISED a grid and the engine handed one
//! back a single backtest at exit 0. This verb promises no such thing — the profile decides — so
//! the same input is now one backtest on both routes, which is the correct answer rather than a
//! lost check. What remains covered is the case that DOES ask for work that cannot happen:
//! `--optimizer` on a profile with no grid, which the engine refuses through
//! `SearchFlags::requested` (#1750), and remotely by ROUTING to the search verb anyway — see
//! [`route_of`] — where the server's own "profile has no \[paramscan\] table" refusal answers on the
//! same rung the local engine's does.
//!
//! # ⚠ A WALK-FORWARD is this verb too, and it is a MODIFIER rather than a run kind (decision 3)
//!
//! ```text
//! vike-cli backtest run --profile <wf.toml> [--addr …]
//! ```
//!
//! There is no `walkforward` verb. It existed until decision 3 of
//! `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`, which folded it inward: a
//! walk-forward is an OPERATION of the compute plane, and the top level names planes. What the
//! profile declares is TWO INDEPENDENT AXES (ruling 7) — a grid section says WHAT is computed, a
//! `[walkforward]` table says HOW it is VALIDATED — and [`route_of`] maps them onto the three wire
//! verbs that carry them. Declaring both COMPOSES: each window re-searches the grid on its own
//! training half, and neither section is ignored or refused (ruling 5).
//!
//! That closes the same class of divergence ruling 13 closed for the grid section — a
//! `[walkforward]` profile run through this verb used to have that table SILENTLY IGNORED, so the
//! same command line answered a different question depending on nothing the output said.
//! `crates/vike-backtest/profiles/wf_grid.toml` states the trap in so many words.
//!
//! ⚠ There is no `--local` for it and that is the ENGINE's property, not this verb's — see
//! [`WALKFORWARD_HAS_NO_LOCAL_ARM`].
//!
//! # `--local`: the same run, its ENGINE on THIS machine — the history still comes from a datahub
//!
//! ```text
//! vike-cli backtest run --local --profile <run.toml> [--engine PATH] [--json]
//! ```
//!
//! The remote path above is the compute-to-data offload and needs a `vike-datahub` to talk to.
//! `--local` is for the box that already HAS the tape: it drives the standalone `backtest` engine
//! — **spawned, never linked**, because this crate's identity is being DataFusion-free
//! ([`crate::cmd::engine`] carries the whole argument and the search order). `--engine` names the
//! engine outright and is refused without `--local`, where it would describe the wrong machine.
//!
//! ⚠ **`--local` moves the ENGINE, not the HISTORY.** Since 2026-09-25 every history read goes
//! through a datahub (decision 0084), so the spawned engine dials `$VIKE_DATAHUB_ADDR` (loopback by
//! default) like every other reader. For tape on this box, start a key-less datahub on it first:
//! `VIKE_DATAHUB_STORE=DIR vike-backend datahub`. `--store DIR` named that directory until then and
//! is now refused BY NAME on both arms, with that command in the refusal
//! (`vike_datahub_client::flag_vocab::store_flag_removed`).
//!
//! ⚠ A release attaches that engine beside `vike-cli` on **Linux only** — no published manifest
//! carries a `backtest.exe` — so on Windows this arm needs an engine the user built or fetched
//! themselves, and the missing-engine message says so. [`crate::cmd::engine`]'s module doc is the
//! authority on that asymmetry and on what would close it; nothing here restates the release's
//! asset list.
//!
//! ⚠ **`--preset` and `--script` behave identically in both modes**, which is the property that
//! makes a local run a rehearsal for a remote one: they are client-side rewrites of the profile
//! TEXT, applied here, and the local arm hands the engine the rewritten text through a staged file
//! rather than asking it to grow flags it does not have. See [`execute_local`].
//!
//! Exit code: `0` when the server returns a report, and otherwise a rung of [`crate::exit`] — `2`
//! for a bad command line, `3` when the datahub could not be reached, `1` for everything else (an
//! unreadable profile or preset, a server-side `Response::Error`). The failure message goes to
//! stderr on every rung.
//!
//! # ⚠ Why the preset is resolved HERE and not named in the profile
//!
//! Only the profile TEXT crosses the wire, and the server is typically a different machine (the CI box,
//! next to the tape). A `[strategy] preset = "fast"` FIELD would therefore be resolved against the
//! SERVER's `user_data/`, not the author's — the presets a person is editing would be invisible to
//! the run they just started. So a preset is a LOCAL file, read and merged before the request, the
//! same shape `--script` already has and for the same reason.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::Value;
use vike_datahub_client::{DatahubClient, WireSearch, flag_vocab};
use vike_node_proto::auth::{NodeKeys, Scope};

use crate::cmd::args::{self, Flags};
use crate::exit::{CliError, CmdResult};

/// The COMPUTE daemon's address, folded from the three rungs ruling 7 names: `--addr <v>` →
/// `settings/config.toml`'s `backtest_addr` → [`vike_config::DEFAULT_BACKTEST_ADDR`].
///
/// ⚠ **This function exists because a literal used to sit where the last rung is.** The verb
/// carried its own `const DEFAULT_ADDR`, which had two costs and the second is the one that
/// matters: an operator's `backtest_addr` was read by nothing here, so a box configured for its
/// compute daemon still dialled the compiled-in address — and a compiled-in address is exactly
/// what goes stale when a port is reassigned. The number now has ONE spelling in the workspace,
/// in `vike-config`, shared with the daemon that binds it.
///
/// A BLANK `--addr`/`backtest_addr` is treated as absent rather than honoured: it can only have
/// come from an unset shell variable or an empty TOML string, neither of which is an address, and
/// dialing `""` would report a connect failure naming nothing. (The file layer already refuses a
/// value with no `:`; the flag has no such check, so this is the one place a `--addr ''` is
/// answered.)
///
/// ⚠ **EVERY compute dialer folds this setting now, and `mcp`'s compute tools were the last that
/// did not.** This paragraph read *"`backtest` and `study` fold their ladder here; `mcp`'s compute
/// tools do not read `config.backtest_addr` at all"*, and it was true: on a box setting
/// `backtest_addr = "127.0.0.1:7881"`, `backtest` and `study` dialled `7881` while an agent driving
/// `mcp`'s `run_backtest` silently dialled `7880`. Stage 7 of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` closed it — §18 row 9 is
/// where it was recorded — by threading `Resolved::backtest_addr` into that arm too; `mcp`'s own
/// `parse_config` applies the same three rungs (`--backtest-addr` → the setting → the constant).
/// The withdrawn claim is kept beside its correction rather than deleted.
///
/// ⚠ `walkforward` was a THIRD offender here until decision 3 of the backtest-CLI-surface design
/// folded that verb into `backtest run`. It did not get FIXED — it stopped existing, and the
/// walk-forward route now folds this ladder like every other `backtest run`. What is left of that
/// module is a renderer that dials nothing.
///
/// The fold lives HERE, in the verb the setting is NAMED for, because two copies that disagreed
/// about a blank or padded rung could aim a verb at the wrong process — `study`'s own module doc
/// argues the digits (`7880` compute, `7878` the data server, `7879` the daemon that signs orders).
/// ⚠ `mcp` is the ONE dialer that folds its own rungs rather than calling this function, and that
/// is a shape rather than a second rule: its ladder is resolved inside `parse_config`, beside the
/// `--addr` that names the DATA daemon, because that server holds tools on both planes and one
/// parser answers for both addresses. Its middle rung is the same `Option<&str>` the dispatcher
/// hands every other arm.
///
/// ⚠ **Read the sentence above as a PROPERTY, and do not re-state it as a count.** It has already
/// been written twice as one and been wrong both times: first "ONE ladder, TWO verbs" (which missed
/// `walkforward`), then "TWO of the THREE compute verbs … and the third is a KNOWN GAP" (which
/// missed `mcp`, whose compute tools have the same hole). A reader greps for who ignores the
/// setting and believes whatever the count rules out.
///
/// `git grep -l DEFAULT_BACKTEST_ADDR -- crates/vike-cli/src` is the roster — every dialer names
/// that constant, and the dispatcher that decides who gets the SETTING is in the answer too. It
/// carries no number, so it cannot rot when another dialer appears, and
/// `crates/vike-ops/tests/unrun_command_gate.rs` RUNS it, so it cannot quietly stop answering
/// either.
///
/// ⚠ `study` does NOT retire into `backtest run`: owner ruling R1 makes it `vike-cli research
/// study`, a sub-verb of the `research` plane, so `crates/vike-cli/src/cmd/study.rs` remains a
/// CALLER and reaches this through the canonical path — there is deliberately no `pub use` shim.
pub(crate) fn resolve_addr(cli: Option<&str>, configured: Option<&str>) -> String {
    for rung in [cli, configured] {
        if let Some(v) = rung
            && !v.trim().is_empty()
        {
            return v.trim().to_string();
        }
    }
    vike_config::DEFAULT_BACKTEST_ADDR.to_string()
}

/// The `[strategy.params]` key carrying a Rhai strategy's SOURCE — what `--script` sets, and the one
/// key a `--preset` file may not define.
///
/// ⚠ **The spelling is `vike_model::RESERVED_SRC_KEY` and this is an ALIAS, not a second copy.**
/// It had three homes — here, `crates/vike-studio-core/src/user_strategies/load.rs` and the bare
/// literal in `crates/vike-backtest/src/harness/registry.rs`'s `rhai_overrides` — and
/// `docs/decisions/0064-a-named-run-carries-no-source.md` needed a fourth reader below all of
/// them, which is what moved the fact down to the one crate every reader can see.
const SRC_KEY: &str = vike_model::RESERVED_SRC_KEY;

/// The container key a preset must NOT wrap its knobs in — see [`merge_preset_params`].
const PARAMS_WRAPPER_KEY: &str = "params";

// ⚠ **`RANK_METRICS` and `OPTIMIZERS` USED TO BE TWO CONSTS HERE, AND THE ROSTERS THEY HELD NOW
// LIVE IN THE PROTOCOL CRATE.** `--rank-by` and `--optimizer` are spelling-checked against
// `vike_datahub_client::flag_vocab::BACKTEST_FLAGS` — through `flag_vocab::accept_value`, on this
// file's two selector arms in `parse_run_args` — so neither the roster nor the MATCH RULE is
// written down twice any more. This block is the tombstone rather than a `pub use`, which this
// workspace refuses on a move; what it keeps is the two arguments that outlived the consts.
//
// **Why the check happens here at all**, which is unchanged: it is a SPELLING check and never a
// second implementation — what a metric or a method MEANS is `vike_backtest::harness`, on
// whichever side runs — and catching a typo before the dial or the spawn is what makes it a local
// usage error instead of a wasted round trip. `crate::cmd::data`'s `ON_GAP_VALUES` states the same
// rule for its own two rosters, in the same words.
//
// **What the two consts cost, kept because each is the reason the shared vocabulary exists:**
//   * `OPTIMIZERS` was a THREE-element array refusing `genetic` during arg parsing while `grid`
//     was the only name with a remote route. Stage 7 of
//     `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` collapsed three rosters
//     into one — the wire carries a method (`Request::RunParamscanProfile`'s `search`) — and the
//     roster moved to the protocol crate because that is the only crate this one and
//     `vike-backtest` both depend on. It had been `= vike_datahub_client::SEARCH_METHODS` ever
//     since, so its MEMBERS could not drift; its MATCH RULE could, and did.
//   * `RANK_METRICS` was a five-element literal to the end, and the drift it hid was the rule
//     rather than the names: `one_of` compared with `contains` — exact, case-sensitive — while
//     `crates/vike-backtest/src/harness/sweep.rs`'s `RankMetric::from_str_ci` lowercases and
//     `crates/vike-backtest/src/harness/search_select.rs`'s `resolve` uses
//     `eq_ignore_ascii_case`. So `--rank-by SHARPE` was a local exit-2 here and a legal engine
//     value one crate over, and `--optimizer TPE` with it. `flag_vocab`'s own doc carries the
//     measurement and argues the direction of the fix: widening the client is the direction in
//     which nothing that works stops working.
//
// ⚠ `multi` reaches BOTH routes since stage 7, and `RANK_METRICS`'s doc said the opposite for a
// while. It used to be LOCAL-ONLY as a WIRE fact: the server resolved `rank_by` through
// `harness::RankMetric::from_str_ci`, whose four arms have no `multi`. It resolves through
// `search_select`'s `resolve_rank` now, which has FIVE, and a daemon too old to do that is refused
// BY NAME through `vike_datahub_client::FEATURE_SEARCH_METHOD` with nothing sent.

/// The default `--optimizer`, spelled through the protocol crate for the same reason the roster is.
///
/// ⚠ **TEST-ONLY, and that is a fact about this side rather than an omission.** This crate
/// substitutes no default: an absent `--optimizer` is forwarded as `None` on BOTH routes, and the
/// side that RUNS resolves it (`vike_backtest::harness::search_select::resolve` against
/// `DEFAULT_SEARCH_METHOD`). A lib-side default here would be a second answer to "what runs when
/// nobody said", which is the class this stage exists to remove. The tests below need the name to
/// build a valid sample argv, so it is gated rather than invented or deleted.
#[cfg(test)]
const DEFAULT_OPTIMIZER: &str = vike_datahub_client::DEFAULT_SEARCH_METHOD;

/// The command's own usage roster. `pub(crate)` so `crate::cmd::mcp`'s
/// `the_instructions_name_only_real_commands` can hold the MCP `instructions` text to the
/// subcommands and flags THIS module actually accepts, rather than to a copy of them.
///
/// ⚠ Every sub-verb row here is also checked against [`SUBCOMMANDS`] by
/// `every_subcommand_is_named_in_usage`, because the MCP gate above is weaker than it looks on
/// this verb: it asserts `USAGE.contains(word)`, and `run` is already a substring of `<run.toml>`
/// — so that gate would pass a USAGE that never advertises the sub-verb at all.
///
/// ⚠ **It advertises a PLANE's worth of sub-verbs, not two**, because several stages landed on this
/// plane at once: stage 3 made the sub-verb MANDATORY and gave the computing half its own token
/// (`run`), stages 4–6 added the verbs that READ and judge what a run left behind, and the
/// authoring three (`templates`/`script-api`/`script-check`) added the ones that answer about a
/// strategy before there is a run at all. ONE usage and one roster — [`SUBCOMMANDS`] — so no
/// refusal can name the set short. ⚠ This doc carried the COUNT until the authoring verbs landed
/// and the count was wrong the same day; `SUBCOMMANDS.len()` is the answer and prose is not allowed
/// to be a second one.
pub(crate) const USAGE: &str = "usage: vike-cli backtest <subcommand> [options]\n\
\n\
  run          compute a backtest. The resolved PROFILE says what, on TWO independent axes: a\n\
           non-empty [paramscan] grid searches the parameter space, a [walkforward] table walks\n\
           the run forward over out-of-sample windows, and a profile declaring both is sent\n\
           WHOLE — that table's own keys say what each window searches\n\
  ls           list what past runs left behind — filtered, ordered and shaped\n\
  show         re-render ONE stored run, or export one of its stored documents raw\n\
  path         print one absolute path inside a run directory, so `$(…)` works\n\
  tag          label a run, note it, or give it a MARK a later comparison can name\n\
  diff         what differed in the INPUTS beside what differed in the OUTPUTS, for two runs\n\
  gate         judge a run against a mark and turn that verdict into an EXIT CODE\n\
  params       print a Rhai script's (or a named strategy's) tunable param(name, default) knobs.\n\
           OFFLINE — no server, no store, no engine\n\
  strategies   the strategy roster the COMPUTE server can run\n\
  templates    the starter Rhai strategies this binary ships — list them, print one, or save one\n\
           to a NEW file. OFFLINE\n\
  script-api   everything an authored Rhai strategy may CALL: the reads, the order verbs, the\n\
           knob, the indicator spellings — and the hooks to put them in. OFFLINE\n\
  script-check compile one Rhai script and answer with the diagnostic and an EXIT CODE, for a save\n\
           hook or a pre-commit. OFFLINE\n\
\n\
       vike-cli backtest run [--profile <run.toml>] [--venue NAME] [--symbol SYM[,SYM]] [--interval 1h] [--from DATE] [--to DATE] [--kind bar|tick] [--strategy NAME] [--cash N] [--fee RATE] [--slippage RATE] [--decide sequential|simultaneous] [--param k=v] [--set key=value] [--preset <p.toml>] [--script <s.rhai>] [--write-profile <out.toml>] [--show-effective] [--addr 127.0.0.1:7880] [--json]\n\
       vike-cli backtest run --local [--profile <run.toml>] [the same profile-building flags] [--engine PATH] [--json]\n\
       vike-cli backtest ls [selector] [--where EXPR] [--sort FIELD] [--limit N] [--cols a,b,c] [--json] [--out FILE]\n\
       vike-cli backtest show <id> [--metrics] [--trades] [--config] [--export trades|equity] [--json] [--out FILE]\n\
       vike-cli backtest show <id> --html [--out sheet.html]   (the tearsheet as one HTML page)\n\
       vike-cli backtest show --metrics-list [--out FILE]   (the metric CATALOG — no run needed)\n\
       vike-cli backtest path <id> [FILE]\n\
       vike-cli backtest tag <run> [--add TAG]... [--note TEXT] [--as NAME] [--json]\n\
       vike-cli backtest diff <a> <b> [--all] [--changed-only] [--trades] [--md] [--json]\n\
       vike-cli backtest gate <run> --against <mark> --fail-if EXPR [--json]\n\
       vike-cli backtest params [--script <s.rhai> | --strategy NAME] [--json]\n\
       vike-cli backtest strategies [--addr 127.0.0.1:7880] [--json]\n\
       vike-cli backtest templates [ID] [--write-strategy <s.rhai>] [--json]\n\
       vike-cli backtest script-api [--json]\n\
       vike-cli backtest script-check --script <s.rhai> [--json]";

/// Which sub-verb ran. Adding one is an arm here, an arm in [`claim_subcommand`], an arm in
/// [`run`]'s dispatch, and a row in [`USAGE`] — and `every_subcommand_is_named_in_usage` plus
/// `every_subcommand_is_reachable_by_the_name_it_advertises` hold the last two honest.
///
/// ⚠ **The reading half is NESTED rather than flattened**, and that is a type-level statement
/// about the two parsers: [`Sub::Run`] takes the profile-building flag line [`parse_run_args`]
/// drains, while every [`ReadSub`] takes a selector plus the reading flags [`parse_read`] drains.
/// Flattening the nine would put one enum in front of two parsers and lose the only distinction
/// that decides which one a command line goes to. [`ReadSub`]'s own doc predicted "these arms join
/// its `Sub` enum" when stage 3 landed — this is that join, with `ReadSub` kept as the INNER
/// roster so the reading plane's arity rules, foreign-flag refusals and tests keep naming one type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sub {
    /// `run` — the one sub-verb that computes. Decision 1 of
    /// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`: `backtest` is a PLANE,
    /// and a plane is an object with sub-verbs.
    Run,
    /// One of the stored-run verbs — [`ReadSub`] is both the roster and the arity rule for all
    /// eight. `params` is one of them: it is the old `--list-params`, re-homed (§8.5), and
    /// decision 11 says a sub-verb is ALWAYS required, so an offline discovery mode could not stay
    /// a bare flag on the plane.
    Read(ReadSub),
}

/// Every sub-verb, in the order [`USAGE`] lists them.
///
/// ⚠ It exists so the "a subcommand is required (…)" refusal is DERIVED rather than typed. The
/// hand-written copy `crate::cmd::data`'s replaced named its roster SHORT, on the verb that
/// deletes; the derivation is what makes that unrepeatable here.
///
/// ⚠ The reading rows are spelled out rather than folded in from [`READ_SUBCOMMANDS`],
/// because a `const` cannot map over a slice. `the_roster_carries_every_reading_subverb` is what
/// stops the two from drifting — the completeness test a hand copy would otherwise owe forever.
const SUBCOMMANDS: &[Sub] = &[
    Sub::Run,
    Sub::Read(ReadSub::Ls),
    Sub::Read(ReadSub::Show),
    Sub::Read(ReadSub::Path),
    Sub::Read(ReadSub::Tag),
    Sub::Read(ReadSub::Diff),
    Sub::Read(ReadSub::Gate),
    Sub::Read(ReadSub::Params),
    Sub::Read(ReadSub::Strategies),
    Sub::Read(ReadSub::Templates),
    Sub::Read(ReadSub::ScriptApi),
    Sub::Read(ReadSub::ScriptCheck),
];

impl Sub {
    /// The name the operator typed, which is also what every refusal message names it by.
    fn as_str(self) -> &'static str {
        match self {
            Sub::Run => "run",
            Sub::Read(r) => r.as_str(),
        }
    }
}

/// The roster as one message fragment — `run | ls | show | …` — so no refusal writes the list down.
fn subcommand_roster() -> String {
    SUBCOMMANDS.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" | ")
}

/// `--list-params` was a MODE FLAG on this verb for months. Decision 11 gives it a sub-verb, and
/// the old spelling answers with the new one rather than with "unknown argument" — the same reason
/// `crate::RETIRED_COMMANDS` exists one level up.
const LIST_PARAMS_RETIRED: &str = "--list-params is now a subcommand: `vike-cli backtest params --script <s.rhai>`. A discovery \
     runs no backtest at all, and decision 11 of the backtest-CLI-surface design gives every \
     action inside this plane exactly one spelling.";

/// Claim the FIRST argv token as the sub-verb, leaving the rest for that sub-verb's own parser.
///
/// ⚠ **There is no bare `vike-cli backtest …`** — decision 11, and it exists so that one action
/// has one spelling. A missing sub-verb prints usage and exits 2, the way `crate::cmd::data`
/// already behaves.
///
/// Three refusals, each a different mistake and each answered differently:
///   * NOTHING — the roster, derived from [`SUBCOMMANDS`];
///   * a `--flag` — say it is a flag rather than calling it an unknown subcommand, which would
///     send an operator looking for a verb spelled `--profile`;
///   * `--list-params` — the one spelling that SHIPPED, answered with its replacement rather than
///     with a shrug. Same doctrine as `crate::RETIRED_COMMANDS` one level up.
///
/// ⚠ The help arm is [`args::help_requested`] and nothing else. It is generic, so `T` is fixed by
/// this function's own `Result<Sub, String>`; NEVER hand-type the sentinel's text, because
/// `args::exit_for_parse_error` compares the message to that exact constant and a near-miss copy
/// prints the internal token to a user's terminal on exit 1 instead of usage on exit 0.
fn claim_subcommand(it: &mut impl Iterator<Item = String>) -> Result<Sub, String> {
    let Some(first) = it.next() else {
        return Err(format!("a subcommand is required ({})", subcommand_roster()));
    };
    match first.as_str() {
        "run" => Ok(Sub::Run),
        "-h" | "--help" | "help" => args::help_requested(),
        "--list-params" => Err(LIST_PARAMS_RETIRED.to_string()),
        other if other.starts_with("--") => Err(format!(
            "{other} is a flag, and a subcommand is required first ({}) — try `vike-cli backtest \
             run {other} …`",
            subcommand_roster()
        )),
        // ⚠ The reading roster is consulted AFTER the `--` guard, deliberately. A token beginning
        // `--` can never be a [`ReadSub`], so the order changes no answer — but putting the lookup
        // first would read as though a flag might resolve to a sub-verb, and this order states the
        // rule the guard above enforces.
        other => match ReadSub::from_token(other) {
            Some(r) => Ok(Sub::Read(r)),
            None => {
                Err(format!("unknown `backtest` subcommand '{other}' ({})", subcommand_roster()))
            }
        },
    }
}

/// The parsed `backtest` command line.
#[derive(Debug)]
struct Args {
    /// The backtest profile `.toml` — a BASE the overrides are applied onto, optional since
    /// stage 2.
    profile_path: Option<String>,
    /// A preset `.toml` whose keys are merged into the shipped profile's `[strategy.params]`.
    preset_path: Option<String>,
    /// An authored Rhai script whose source is injected into the shipped profile's
    /// `[strategy.params].src`. (Listing a script's KNOBS is `backtest params`, a different
    /// sub-verb with its own parser.)
    script_path: Option<String>,
    /// `--addr <host:port>`: the COMPUTE daemon this run dials, UNRESOLVED. The parser cannot see
    /// `config.backtest_addr` — only the composition root does — so the ladder is folded by
    /// [`resolve_addr`] inside `execute`, and never here.
    addr: Option<String>,
    json: bool,
    /// `--local`: run on THIS machine by driving the standalone engine, instead of shipping the
    /// profile to the compute daemon. It moves the ENGINE, not the history: the child still reads
    /// through a datahub (decision 0084). See [`execute_local`].
    local: bool,
    /// `--engine PATH`: name the standalone engine outright instead of searching for it. See
    /// [`crate::cmd::engine`]'s search order, and why this flag exists rather than a variable.
    engine: Option<String>,
    /// `--rank-by`: which metric orders a parameter search's rows. `None` = whichever side runs it
    /// applies its own default (annualized Sharpe).
    ///
    /// ⚠ It names how to ORDER results, not what work to do, so it is IGNORED — not an error — on
    /// a profile with no `[sweep]` table. That is documented engine behaviour
    /// (`crates/vike-backtest/src/backtest_cli.rs`'s `parse_search_flags`) and this side does not
    /// second-guess it.
    ///
    /// ⚠ CANONICAL, for [`Args::optimizer`]'s reason — `--rank-by SHARPE` is held here as
    /// `sharpe`, so the token that reaches the wire, the spawned engine and the run ARTIFACT's
    /// identity is the one spelling. (`vike_datahub_client::client`'s `run_paramscan_profile`
    /// needed no help for its own `multi` capability pre-check: that one already compares with
    /// `eq_ignore_ascii_case`. What canonicalising buys is every reader FURTHER down agreeing
    /// about what was asked for.)
    rank_by: Option<String>,
    /// `--optimizer`: the search METHOD, spelling-checked against
    /// `vike_datahub_client::flag_vocab`'s `--optimizer` row and otherwise forwarded. `None` is
    /// forwarded as `None` on BOTH routes — this side substitutes no default, because the side
    /// that RUNS resolves it against the protocol's own `DEFAULT_SEARCH_METHOD` and a second
    /// answer here could disagree with it.
    ///
    /// ⚠ **It holds the CANONICAL spelling, not the token that was typed**, and that is the half
    /// of the case fix that finishes it: `flag_vocab::accept_value` lower-cases a roster member
    /// before this field is written, so `--optimizer TPE` reaches the wire and the spawned engine
    /// as `tpe`. Forwarding the typing instead would leave the canonicalisation to happen twice in
    /// two places, one of which is a daemon that may be older than this fix.
    ///
    /// ⚠ Unlike `--rank-by` this says what WORK to do, which is why a profile with no `[paramscan]`
    /// table is ROUTED to the search verb anyway ([`route_of`]) and refused there by the side that
    /// runs, rather than being quietly turned into one backtest.
    optimizer: Option<String>,
    /// The three per-method knobs, forwarded VERBATIM and judged by the engine.
    ///
    /// ⚠ Deliberately not validated here beyond presence. The engine owns the ownership rule (a
    /// `--trials` under `--optimizer euler` is REFUSED, by a table), the ranges and the caps, and
    /// each of its refusals names the method that owns the knob — which is a better message than
    /// anything a second copy of that table could produce. The module doc's "what is validated
    /// here" rule, applied to a fourth family.
    euler_depth: Option<String>,
    trials: Option<String>,
    seed: Option<String>,
    /// Every `--set`, and (from Task 5) every named sugar flag and implied default, already typed
    /// and resolved to the dotted profile key it lands on.
    ///
    /// ⚠ This is where the inversion lives: `--profile` is now a BASE the overrides are applied
    /// onto, rather than the whole input. `execute` hands this list and the (optional) file text to
    /// [`build_profile_toml`], and everything below that point — `--preset`, `--script`, the
    /// `[sweep]` routing decision, `--local`, `--addr` — sees only the resulting TEXT.
    overrides: Vec<Override>,
    /// `--write-profile PATH`: write the profile these flags built, then run (spec §5.3).
    /// ⚠ Refuses an EXISTING path. This is the one file this verb writes, and clobbering a
    /// committed profile from a flag typo is unrecoverable.
    write_profile: Option<String>,
    /// `--show-effective`: print the resolved profile with each override's origin, then STOP —
    /// exit 0, no dial, no engine, no store.
    show_effective: bool,
}

impl Args {
    /// Whether argv asked for a SEARCH — an explicit method, or any method-owned knob.
    ///
    /// ⚠ `rank_by` is deliberately excluded, exactly as `vike_backtest`'s `SearchFlags::requested`
    /// excludes it: it names how to ORDER results, not what work to do, and its ignore on a profile
    /// with no `[paramscan]` table is documented behaviour on both sides.
    fn search_requested(&self) -> bool {
        self.optimizer.is_some()
            || self.euler_depth.is_some()
            || self.trials.is_some()
            || self.seed.is_some()
    }

    /// The selection this command line asks the SERVER for, as TOKENS.
    ///
    /// ⚠ Nothing is parsed here and nothing may be. `vike_backtest::harness::search_select` is the
    /// one authority for what `--trials abc` means and for the sentence that refuses it, and this
    /// crate cannot call it: it has no `vike-backtest` dependency, deliberately. Forwarding the
    /// token is what keeps ONE message on both routes — the `--local` arm already forwards these
    /// five flags verbatim to the spawned engine for exactly the same reason.
    ///
    /// ⚠ **This said "the TOKENS the operator typed" and that is now true of the three KNOBS
    /// only.** `--optimizer` is a member of a declared roster, so `flag_vocab::accept_value`
    /// canonicalises it at the parse arm and this carries `tpe` for a typed `TPE`. The rule is not
    /// "some flags are rewritten": a flag whose values are ENUMERATED has one canonical spelling
    /// and this side may settle it, while a flag whose value is a NUMBER or a PATH has no roster to
    /// canonicalise against and rewriting it would be the parsing this doc refuses.
    fn wire_search(&self) -> Option<WireSearch> {
        self.search_requested().then(|| WireSearch {
            optimizer: self.optimizer.clone(),
            euler_depth: self.euler_depth.clone(),
            trials: self.trials.clone(),
            seed: self.seed.clone(),
        })
    }
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `backtest` verb;
/// `project_root` is `<project>`, resolved once by [`crate::run`] — `--local` needs it to find the
/// standalone engine under `bin/` and to stage a rewritten profile under `tmp/`, and it arrives as
/// a PARAMETER because a `src/cmd/` file may not read the environment for itself. `user_data_dir`
/// is `<project>/user_data`, resolved by the same dispatcher (`Resolved::user_data_dir`: the
/// `$VIKE_USER_DATA_DIR` override, else the sibling of the settings directory the boot resolved),
/// for the same reason.
///
/// # ⚠ ONE directory for the reader AND the writer, by construction
///
/// It used to arrive as a pre-joined `runs_root` (plus its `marks_root` sibling) for the READING
/// half alone, while the `--local` child resolved its own runs root by walking up from its working
/// directory — a walk that does not read `$VIKE_SETTINGS_DIR`. So with that variable naming one
/// project and the operator standing in another, `run --local` saved where `ls` did not look.
/// Now both halves come off this ONE value: the reading verbs join the runs and marks roots onto
/// it here, and `--local` hands the same directory to the child
/// (`crate::cmd::engine`'s `Engine::with_user_data_dir`, which argues the rest).
///
/// ⚠ The marks root is a SIBLING of the runs root and never a child: a `marks/` directory under
/// `runs/` is, to every scan of that tree, a run holding no manifest — a permanent "this run never
/// finished writing" row in every listing. `vike_model::state_path::MARKS_SUBDIR` carries the
/// argument.
///
/// ⚠ It claims a SUB-VERB first ([`claim_subcommand`]) and routes the tail to that sub-verb's own
/// parser. `crate::cmd::trade`'s one-shot verbs are the in-crate precedent for a parser per verb;
/// what is copied from `crate::cmd::data` instead is the one property that matters most — the
/// missing-sub-verb refusal is DERIVED from [`SUBCOMMANDS`], so it can never name the roster short.
pub fn run(
    args: impl Iterator<Item = String>,
    project_root: Option<&Path>,
    user_data_dir: Option<&Path>,
    now: i64,
    configured_addr: Option<&str>,
    keys: Option<&NodeKeys>,
) -> ExitCode {
    let runs_root = user_data_dir.map(|d| d.join(vike_model::state_path::RUNS_SUBDIR));
    let marks_root = user_data_dir.map(|d| d.join(vike_model::state_path::MARKS_SUBDIR));
    // ⚠ The argv is COLLECTED before the sub-verb is claimed, because the two parsers want
    // different slices of it: [`parse_run_args`] drains the TAIL (the token is consumed), while
    // [`parse_read`] takes the WHOLE line and resolves the same first token itself. Re-reading one
    // token is the price of letting the reading plane keep one entry point that its own unit tests
    // drive directly — `parse_read(&["ls", …])` is how every one of them is written.
    let argv: Vec<String> = args.collect();
    let mut it = argv.iter().cloned();
    let sub = match claim_subcommand(&mut it) {
        Ok(s) => s,
        Err(msg) => return args::exit_for_parse_error("backtest", USAGE, &msg),
    };
    match sub {
        Sub::Run => {
            let args = match parse_run_args(it) {
                Ok(a) => a,
                Err(msg) => return args::exit_for_parse_error("backtest run", USAGE, &msg),
            };
            match execute(&args, project_root, user_data_dir, configured_addr, keys) {
                Ok(()) => ExitCode::SUCCESS,
                // The message is printed exactly as it always was; only the NUMBER beside it is
                // new. See [`crate::exit`] for what each rung licenses a caller to do about it.
                Err(e) => {
                    eprintln!("vike-cli backtest run: {}", e.msg);
                    e.exit.into()
                }
            }
        }
        // ⚠ The whole `argv` goes to [`parse_read`], token included — `it` is deliberately unused
        // on this arm. That parser resolves the sub-verb itself so that every one of its unit
        // tests can drive it with a complete command line, which is the form an operator types.
        Sub::Read(_) => {
            let parsed = match parse_read(&argv.iter().map(String::as_str).collect::<Vec<_>>()) {
                Ok(a) => a,
                Err(msg) => return args::exit_for_parse_error("backtest", USAGE, &msg),
            };
            let ctx = crate::cmd::runs::Ctx {
                runs_root: runs_root.as_deref(),
                marks_root: marks_root.as_deref(),
                configured_addr,
                keys,
            };
            match execute_read(&parsed, &ctx, now) {
                Ok(exit) => exit.into(),
                Err(e) => {
                    eprintln!("vike-cli backtest {}: {}", parsed.sub.as_str(), e.msg);
                    e.exit.into()
                }
            }
        }
    }
}

/// PARSE ONLY: does `backtest`'s real grammar accept this argv tail — everything after
/// `backtest`? Routed exactly as [`run`] routes it (claim the sub-verb, then that sub-verb's own
/// parser), so a line this answers `Ok` for is a line the binary would get as far as EXECUTING.
/// For tests in sibling modules that print a `vike-cli backtest …` line; `crate::cmd::accepts` is
/// the entry they call.
#[cfg(test)]
pub(super) fn accepts(argv: &[&str]) -> Result<(), String> {
    let mut it = argv.iter().map(|s| s.to_string());
    match claim_subcommand(&mut it)? {
        Sub::Run => parse_run_args(it).map(|_| ()),
        Sub::Read(_) => parse_read(argv).map(|_| ()),
    }
}

/// The STORED-RUN sub-verbs, which take a sub-verb TOKEN rather than flags.
///
/// ⚠ **Spec decision 11 — the sub-verb is ALWAYS required — HAS landed**, and this doc said the
/// opposite until it did. Stage 3 of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` deleted the bare flag form
/// (`backtest --profile run.toml` is now an exit-2 naming the roster), and what this enum predicted
/// then happened: these arms joined [`Sub`], as its [`Sub::Read`] payload. The PEEK is gone —
/// [`claim_subcommand`] is the one router, and it resolves a reading token through
/// [`ReadSub::from_token`] rather than by re-listing the names.
///
/// It stayed a SEPARATE enum rather than being flattened into [`Sub`] because the reading plane has
/// its own arity rule, its own foreign-flag refusals and its own parser, all of which name this
/// type; [`Sub`]'s own doc carries that argument.
///
/// ⚠ **The name says READ and two members WRITE.** [`ReadSub::Tag`] appends to a run's optional
/// sidecar and to the marks store; it touches no manifest and no report, so nothing it writes can
/// make a run unreadable — which is what lets it sit in this family.
/// `crate::cmd::runs`'s module doc is where that exception is argued. [`ReadSub::Templates`] is the
/// second: it CREATES a `.rhai` file, and it may create only one that does not exist yet
/// (`crate::cmd::strategies`'s `run_templates` argues the no-clobber rule), so like `tag` it can
/// make nothing unreadable.
///
/// ⚠ **The name also says RUN, and the authoring three read no run at all.** `templates` and
/// `script-api` answer from consts compiled into this binary and `script-check` from one file the
/// operator named — no socket, no engine, no store, and no run directory either. They are in this
/// family because they share its parser, its arity rule and its foreign-flag refusals, and they
/// live in `crate::cmd::strategies` rather than under `crate::cmd::runs` because that module's
/// identity is the run directory. Its module doc carries the whole argument, including why each is
/// a sub-verb rather than a flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadSub {
    /// `ls [selector]` — the registry listing, the leaderboard and the job list, one verb.
    Ls,
    /// `show <id>` — re-render one stored run with no recompute.
    Show,
    /// `path <id> [FILE]` — one absolute path on stdout and nothing else, so `$(…)` works.
    Path,
    /// `tag <run>` — labels, notes and MARKS. The one member of this family that writes.
    Tag,
    /// `diff <a> <b>` — what differed in the INPUTS beside what differed in the OUTPUTS.
    Diff,
    /// `gate <run> --against <mark> --fail-if EXPR` — the verb whose product is an EXIT CODE.
    Gate,
    /// `params` — a strategy's or a script's tunable knobs, OFFLINE.
    Params,
    /// `strategies` — the roster the SERVER can run. The one reading verb that opens a socket.
    Strategies,
    /// `templates [ID]` — the starter Rhai strategies this binary ships, listed, printed, or saved
    /// to a NEW file. The one member of this family that CREATES a file, and it refuses to
    /// overwrite one.
    Templates,
    /// `script-api` — everything an authored Rhai strategy may CALL, derived from the host's own
    /// registration rather than from a list.
    ScriptApi,
    /// `script-check --script <s.rhai>` — compile one offline. The SECOND member of this family
    /// whose product is an exit code (`gate` is the first), which is why [`execute_read`] returns
    /// a rung.
    ScriptCheck,
}

/// Every stored-run sub-verb, in the order [`USAGE`] lists them. DERIVED into every message that
/// names the roster — `crate::cmd::data`'s `SUBCOMMANDS` doc carries what the hand-typed copy cost
/// there.
pub(crate) const READ_SUBCOMMANDS: &[ReadSub] = &[
    ReadSub::Ls,
    ReadSub::Show,
    ReadSub::Path,
    ReadSub::Tag,
    ReadSub::Diff,
    ReadSub::Gate,
    ReadSub::Params,
    ReadSub::Strategies,
    ReadSub::Templates,
    ReadSub::ScriptApi,
    ReadSub::ScriptCheck,
];

impl ReadSub {
    /// The name the operator typed, which is also what every refusal names it by.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ReadSub::Ls => "ls",
            ReadSub::Show => "show",
            ReadSub::Path => "path",
            ReadSub::Tag => "tag",
            ReadSub::Diff => "diff",
            ReadSub::Gate => "gate",
            ReadSub::Params => "params",
            ReadSub::Strategies => "strategies",
            ReadSub::Templates => "templates",
            // ⚠ HYPHENATED, and the first two on this plane. `crate::cmd::data`'s
            // `fetch-starter`/`seed-demo` are the in-crate precedent. A bare `api` would read as
            // "the backtest plane's API" — which is not what it answers — and a bare `check` as
            // "check the profile"; both name the SCRIPT, so both say so.
            ReadSub::ScriptApi => "script-api",
            ReadSub::ScriptCheck => "script-check",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        READ_SUBCOMMANDS.iter().copied().find(|s| s.as_str() == token)
    }
}

/// The parsed reading command line. Every field is shared across the reading sub-verbs and
/// refused by name where it does not apply — [`refuse_foreign_read_flags`], and the reason each
/// carries.
#[derive(Debug)]
pub(crate) struct ReadArgs {
    pub(crate) sub: ReadSub,
    /// The selector (`ls` filter, `show`/`path`/`tag`/`gate` target, `diff`'s LEFT operand). See
    /// `crate::cmd::runs::selector`.
    pub(crate) selector: Option<String>,
    /// `path`'s optional second positional: which FILE inside the run directory. Also `diff`'s RIGHT
    /// operand — one field rather than two, because in both cases it is "the second positional" and
    /// a second field would need the same per-sub-verb arity rule to decide which was populated.
    pub(crate) file: Option<String>,
    pub(crate) json: bool,
    pub(crate) out: Option<String>,
    // ls
    pub(crate) where_expr: Option<String>,
    pub(crate) sort: Option<String>,
    pub(crate) limit: Option<String>,
    pub(crate) cols: Option<String>,
    // show
    pub(crate) metrics: bool,
    /// `show --metrics-list` — print the METRIC CATALOG (every id, where it is stored, what it
    /// measures, and the declared-absent rows) and stop. It answers a question about the CATALOG
    /// rather than about a run, which is why it takes no selector and reads no runs directory:
    /// `vike_analytics::metric_catalog::metric_list_text` needs neither.
    ///
    /// ⚠ **It is the command THREE shipped refusals in that module already name**, and until this
    /// field existed those three told an operator to run something that exited "unknown option" —
    /// `parse_metric_selection`'s own doc measured that and called wiring the parser without this
    /// flag "the trap". This is the half of that debt this change pays; the SELECTION half
    /// (`--metrics` widened to take a value) is argued there and deliberately not ridden in here.
    pub(crate) metrics_list: bool,
    /// `show --html`: this run's tearsheet as a standalone HTML DOCUMENT, to `--out` or
    /// stdout.
    ///
    /// ⚠ It was a member of `crate::cmd::runs::show`'s `UNBUILT_RENDERERS` until this change,
    /// refused with a sentence blaming a missing renderer. The renderer was never missing:
    /// `vike_analytics::render_html` is exported under no feature at all, and
    /// `vike_analytics::LiveTearsheet::from_report`'s doc names `show.rs` by path as the caller it
    /// was built for. What was missing was the EDGE — a vike-report edge when this flag shipped,
    /// and since 2026-09-28 no new edge at all: both moved into vike-analytics, which this crate
    /// already linked for the metric catalog.
    ///
    /// ⚠ A BARE switch, not `--html PATH`: `--out` is already this verb's destination for a
    /// document (`--export` writes through it and `--metrics-list` composes with it), so a path
    /// on the flag would be a second spelling of the same thing. §6.2 spells `--html FILE`; this
    /// deviates deliberately and consistently with the two siblings in the same function.
    pub(crate) html: bool,
    /// `show --trades` renders the ledger; `diff --trades` adds it as a third diff section. Shared
    /// deliberately: it names the same document in both, and a `--trades` that meant two things
    /// would be the drift this one flag loop exists to prevent.
    pub(crate) trades: bool,
    pub(crate) config: bool,
    /// `--export VALUE` — one of `vike_model::runs`'s stored documents, raw. See
    /// `crate::cmd::runs::show`'s `export_document` for which values are served and why `fills`
    /// is refused by NAME rather than by refusing the flag.
    pub(crate) export: Option<String>,
    // tag
    /// `--add TAG`, REPEATABLE. A label on the run, in its own sidecar.
    pub(crate) add: Vec<String>,
    /// `--note TEXT`. Appended, never replacing — `vike_model::runs::add_tags` owns that rule.
    pub(crate) note: Option<String>,
    /// `--as NAME`. The MARK — the stable second operand `gate --against` takes.
    pub(crate) mark_as: Option<String>,
    // gate
    /// `--against <selector>`, REQUIRED by `gate`. Usually a mark, because a run id moves.
    pub(crate) against: Option<String>,
    /// `--fail-if EXPR`, REQUIRED by `gate`. See `crate::cmd::runs::failif`.
    pub(crate) fail_if: Option<String>,
    // diff
    /// `--all`: show unchanged leaves too. The negation of the default.
    pub(crate) all: bool,
    /// `--changed-only`: the DEFAULT, accepted so a script that says what it means is not refused.
    pub(crate) changed_only: bool,
    /// `--md`: the same rows as a Markdown table.
    pub(crate) md: bool,
    // params
    pub(crate) script: Option<String>,
    pub(crate) strategy: Option<String>,
    // strategies
    pub(crate) addr: Option<String>,
    // templates
    /// `--write-strategy FILE`: save the named starter to a file that does not exist yet.
    ///
    /// ⚠ **It is not `--out`, and that is a correctness choice rather than a naming one.** `--out`
    /// OVERWRITES on `ls` and `show` (`crate::surface`'s own row says so), and the file this one
    /// writes is a strategy somebody then edits — clobbering that from a flag typo is
    /// unrecoverable. So it takes `--write-profile`'s rule (refuse an existing path) and, because
    /// one flag may not carry two clobber policies, it takes its own name too.
    pub(crate) write_strategy: Option<String>,
}

impl ReadArgs {
    /// Every flag at its default, for one sub-verb. [`parse_read`] fills it in and a unit test builds
    /// one directly — a struct literal in both places is two rosters that drift the day a field is
    /// added.
    pub(crate) fn empty(sub: ReadSub) -> Self {
        Self {
            sub,
            selector: None,
            file: None,
            json: false,
            out: None,
            where_expr: None,
            sort: None,
            limit: None,
            cols: None,
            metrics: false,
            metrics_list: false,
            html: false,
            trades: false,
            config: false,
            export: None,
            add: Vec::new(),
            note: None,
            mark_as: None,
            against: None,
            fail_if: None,
            all: false,
            changed_only: false,
            md: false,
            script: None,
            strategy: None,
            addr: None,
            write_strategy: None,
        }
    }
}

/// ONE flag loop, then per-sub-verb refusals — the shape `crate::cmd::data`'s `parse` has.
fn parse_read(argv: &[&str]) -> Result<ReadArgs, String> {
    let mut it = argv.iter().map(|s| (*s).to_string());
    let first = it.next().ok_or_else(|| {
        format!(
            "a subcommand is required ({})",
            READ_SUBCOMMANDS.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" | ")
        )
    })?;
    let sub = ReadSub::from_token(&first)
        .ok_or_else(|| format!("unknown `backtest` subcommand '{first}'"))?;

    let mut a = ReadArgs::empty(sub);
    let mut positionals: Vec<String> = Vec::new();

    let mut flags = Flags::new(it);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--json" => {
                args::no_value(&flag, inline)?;
                a.json = true;
            }
            "--out" => a.out = Some(flags.value(&flag, inline)?),
            "--where" => a.where_expr = Some(flags.value(&flag, inline)?),
            "--sort" => a.sort = Some(flags.value(&flag, inline)?),
            "--limit" => a.limit = Some(flags.value(&flag, inline)?),
            "--cols" => a.cols = Some(flags.value(&flag, inline)?),
            "--metrics" => {
                args::no_value(&flag, inline)?;
                a.metrics = true;
            }
            // ⚠ **The catalog LISTING, and it must sit as its own arm rather than fall anywhere
            // near `--metrics`.** The two are different questions — one selects a SECTION of a
            // stored run, the other enumerates what this tree can measure — and the guard below
            // that refuses their combination is what stops the second silently winning over the
            // first. It is exact-token like every arm in this loop, so `--metrics` does not answer
            // for it and it does not answer for `--metrics`.
            "--metrics-list" => {
                args::no_value(&flag, inline)?;
                a.metrics_list = true;
            }
            // ⚠ A DOCUMENT flag, like `--export` and `--json`, which is why the guard below
            // refuses their combinations rather than this loop ordering them. Bare: the
            // destination is `--out`, and an inline `--html=path` is a usage error rather than a
            // silently ignored value.
            "--html" => {
                args::no_value(&flag, inline)?;
                a.html = true;
            }
            "--trades" => {
                args::no_value(&flag, inline)?;
                a.trades = true;
            }
            "--export" => a.export = Some(flags.value(&flag, inline)?),
            "--config" => {
                args::no_value(&flag, inline)?;
                a.config = true;
            }
            // ⚠ REPEATABLE, and the only repeatable flag in this loop: `--add ci --add fee-fix` is
            // two labels, not the second overwriting the first.
            "--add" => a.add.push(flags.value(&flag, inline)?),
            "--note" => a.note = Some(flags.value(&flag, inline)?),
            "--as" => a.mark_as = Some(flags.value(&flag, inline)?),
            "--against" => a.against = Some(flags.value(&flag, inline)?),
            "--fail-if" => a.fail_if = Some(flags.value(&flag, inline)?),
            "--all" => {
                args::no_value(&flag, inline)?;
                a.all = true;
            }
            "--changed-only" => {
                args::no_value(&flag, inline)?;
                a.changed_only = true;
            }
            "--md" => {
                args::no_value(&flag, inline)?;
                a.md = true;
            }
            "--script" => a.script = Some(flags.value(&flag, inline)?),
            "--strategy" => a.strategy = Some(flags.value(&flag, inline)?),
            "--addr" => a.addr = Some(flags.value(&flag, inline)?),
            "--write-strategy" => a.write_strategy = Some(flags.value(&flag, inline)?),
            // ⚠ The §6.2 flags this stage cannot honour, refused BY NAME rather than falling into
            // "unknown option". They are in the design document an operator is reading from, so
            // "unknown option --html" would be a message about the wrong thing.
            //
            // ⚠ The ROSTER is `crate::cmd::runs::show`'s `UNBUILT_RENDERERS`, matched here rather
            // than re-typed as a pattern: that module owns both the list and the sentence for each
            // entry, and a literal `"--html" | "--breakdown" | …` here would be a second copy that
            // could fall behind the day one of them ships.
            //
            // ⚠ **THIS GUARD IS LAST, AND THE LITERAL ARMS ABOVE IT WIN.** `--metrics`, `--trades`,
            // `--config` and `--export` are matched by name earlier in this same `match`, so a flag
            // put BACK on `UNBUILT_RENDERERS` while its literal arm survives would never reach here:
            // the refusal becomes dead code and the flag silently no-ops. Retiring a flag is
            // therefore TWO edits — add the row there, delete the arm here — and
            // `an_unbuilt_renderer_is_refused_with_what_it_would_need` is what catches the pair
            // coming apart, because it drives every roster entry through this parser.
            other if crate::cmd::runs::show::UNBUILT_RENDERERS.contains(&other) => {
                return Err(crate::cmd::runs::show::refuse_an_unbuilt_renderer(other));
            }
            // ⚠ The RUN-ONLY flags, refused by name on `params` alone — stage 3's tightening, kept
            // when its `parse_params_args` was folded into this loop. It is scoped to that one
            // sub-verb because that is the scope stage 3 argued for and measured; every other
            // reading verb still answers "unknown option", unchanged. Same placement rule as the
            // guard above: AFTER the literal arms, so `--strategy` and `--json` — which this
            // sub-verb accepts — are taken by their own arms and never reach here.
            other if a.sub == ReadSub::Params && PARAMS_REFUSED.contains(&other) => {
                return Err(refuse_a_run_flag_on_params(other));
            }
            "-h" | "--help" => return args::help_requested(),
            other if other.starts_with("--") => return Err(format!("unknown option '{other}'")),
            positional => match inline {
                // A positional carrying an `=` was split by the shared flag iterator; put it back
                // rather than silently truncating a selector somebody typed.
                Some(v) => positionals.push(format!("{positional}={v}")),
                None => positionals.push(positional.to_string()),
            },
        }
    }

    let allowed = match sub {
        ReadSub::Ls | ReadSub::Show | ReadSub::Tag | ReadSub::Gate => 1,
        ReadSub::Path | ReadSub::Diff => 2,
        ReadSub::Params | ReadSub::Strategies => 0,
        // `templates` takes an OPTIONAL starter id: absent is the roster, present is that
        // starter. ⚠ `script-check` takes NONE and names its file with `--script`, the same
        // spelling `params` uses for the same thing — a second grammar for "which .rhai file" is
        // exactly what decision 11 refuses across this plane.
        ReadSub::Templates => 1,
        ReadSub::ScriptApi | ReadSub::ScriptCheck => 0,
    };
    if positionals.len() > allowed {
        return Err(format!(
            "unexpected extra argument '{}' — `backtest {}` takes {allowed} positional argument(s)",
            positionals[allowed],
            sub.as_str()
        ));
    }
    match sub {
        // `ls` takes an OPTIONAL selector: absent is every run.
        ReadSub::Ls => a.selector = positionals.first().cloned(),
        // ⚠ `show --metrics-list` takes NO selector, and that is the flag's whole shape: it answers
        // out of the metric CATALOG, so there is no run for a selector to name and no runs
        // directory to read. `templates` makes the same move for the same reason — requiring a
        // positional would put the listing behind a run, on the box where somebody is deciding
        // what to type. A selector given anyway is kept and then unused, because the listing is the
        // answer either way, and refusing it would be a refusal about a token rather than about
        // anything an operator did wrong.
        ReadSub::Show if a.metrics_list => a.selector = positionals.first().cloned(),
        ReadSub::Show | ReadSub::Tag | ReadSub::Gate => {
            a.selector = Some(required_selector(sub, positionals.first())?);
        }
        ReadSub::Path => {
            a.selector = Some(required_selector(sub, positionals.first())?);
            a.file = positionals.get(1).cloned();
        }
        // ⚠ `diff` takes TWO runs and neither is optional. A one-operand `diff` that silently used
        // `@last` as the other side would compare against whatever happened to run most recently,
        // which is the "silent precedence" the selector grammar refuses everywhere else.
        ReadSub::Diff => {
            a.selector = Some(required_selector(sub, positionals.first())?);
            a.file = Some(
                positionals.get(1).filter(|s| !s.trim().is_empty()).cloned().ok_or_else(|| {
                    "`backtest diff` compares TWO runs — `backtest diff <a> <b>`. Name both; \
                         there is no implied second operand."
                        .to_string()
                })?,
            );
        }
        // ⚠ `templates` shares `ls`'s rule rather than `show`'s: an absent positional is the WHOLE
        // roster, not a missing operand. Requiring one would make the listing unreachable, and the
        // listing is the half no competitor's `new-strategy` prints.
        ReadSub::Templates => a.selector = positionals.first().cloned(),
        ReadSub::Params | ReadSub::Strategies | ReadSub::ScriptApi | ReadSub::ScriptCheck => {}
    }
    refuse_foreign_read_flags(&a)?;
    if sub == ReadSub::Show {
        crate::cmd::runs::show::refuse_a_listing_beside_a_run_rendering(&a)?;
        // ⚠ The SECOND of the two document rules, and both are needed because they answer
        // different questions: the one above refuses a CATALOG listing beside a run rendering,
        // this one refuses two RUN documents. They share
        // `crate::cmd::runs::show::run_rendering_flags_given`, which is what stops either from
        // being the file that forgets a flag the other knows about.
        crate::cmd::runs::show::refuse_a_second_document(&a)?;
    }
    if sub == ReadSub::Tag {
        crate::cmd::runs::tag::refuse_a_tag_that_writes_nothing(&a)?;
    }
    if sub == ReadSub::Gate {
        crate::cmd::runs::gate::refuse_an_ungateable_line(&a)?;
    }
    Ok(a)
}

fn required_selector(sub: ReadSub, given: Option<&String>) -> Result<String, String> {
    match given {
        Some(s) if !s.trim().is_empty() => Ok(s.clone()),
        _ => Err(format!(
            "`backtest {}` requires a run selector — one of: <run-id> | <unique-prefix> | @last | \
             @last:<kind>",
            sub.as_str()
        )),
    }
}

/// A flag that exists on a SIBLING sub-verb is refused by name with the reason, never dropped —
/// `crate::cmd::data`'s `refuse_foreign_flags` carries the argument: an operator who typed it is not
/// looking for "unknown option", they want the verb that takes it.
fn refuse_foreign_read_flags(a: &ReadArgs) -> Result<(), String> {
    let ls_only = [
        ("--where", a.where_expr.is_some()),
        ("--sort", a.sort.is_some()),
        ("--limit", a.limit.is_some()),
        ("--cols", a.cols.is_some()),
    ];
    // ⚠ `--trades` is NOT here — `show` and `diff` both own it, so it is refused below by its own
    // named rule rather than by this roster. `--export` IS here: `show` alone serves a stored
    // document raw.
    // ⚠ `--metrics-list` is deliberately NOT here, and this roster's own SENTENCE is why: it says
    // the flag "selects a SECTION of one stored run", which is true of these three and FALSE of a
    // listing — that flag selects nothing and reads no run at all. A refusal that is untrue of the
    // flag it lands on is worse than a generic one, so it gets a named rule below. Note the reason
    // differs from `--trades`' and `--script`'s named rules (two sub-verbs own those); this one is
    // owned by `show` alone and still needs its own words.
    let show_only =
        [("--metrics", a.metrics), ("--config", a.config), ("--export", a.export.is_some())];
    let tag_only =
        [("--add", !a.add.is_empty()), ("--note", a.note.is_some()), ("--as", a.mark_as.is_some())];
    let gate_only = [("--against", a.against.is_some()), ("--fail-if", a.fail_if.is_some())];
    let diff_only = [("--all", a.all), ("--changed-only", a.changed_only), ("--md", a.md)];
    // ⚠ `--script` LEFT this roster when `script-check` shipped: two sub-verbs own it now, so it is
    // refused below by its own named rule, exactly as `--trades` is. A flag refused twice with two
    // messages is a flag whose two refusals can disagree.
    let params_only = [("--strategy", a.strategy.is_some())];
    let strategies_only = [("--addr", a.addr.is_some())];
    let templates_only = [("--write-strategy", a.write_strategy.is_some())];

    let refuse = |set: &[(&str, bool)], why: &str| -> Result<(), String> {
        for (flag, given) in set {
            if *given {
                return Err(format!(
                    "{flag} does not apply to `backtest {}` — {why}",
                    a.sub.as_str()
                ));
            }
        }
        Ok(())
    };
    if a.sub != ReadSub::Ls {
        refuse(&ls_only, "that flag narrows, orders or shapes a LISTING, and `ls` is the listing")?;
    }
    if a.sub != ReadSub::Show {
        refuse(
            &show_only,
            "that flag selects a SECTION of one stored run, which is what `show` renders",
        )?;
    }
    // ⚠ `--metrics-list` is refused by a NAMED rule for a reason none of the other named rules
    // share: the `show_only` sentence above is FALSE of it. That roster tells an operator the flag
    // "selects a SECTION of one stored run", and this one prints the metric CATALOG — no run, no
    // section, no runs directory. Sending somebody to `show` is right; telling them why in words
    // that do not describe their flag is how a refusal stops being evidence for anything.
    // ⚠ `--html` gets a NAMED rule for the same reason `--metrics-list` does: the `show_only`
    // sentence above says the flag "selects a SECTION of one stored run", and this one renders the
    // WHOLE run as a single document. Sending somebody to `show` is right; describing their flag
    // wrongly on the way is how a refusal stops being evidence.
    if a.sub != ReadSub::Show && a.html {
        return Err(format!(
            "--html does not apply to `backtest {}` — it renders ONE stored run's whole tearsheet              as an HTML page, which is `show`'s answer beside its --metrics: `backtest show <run>              --html --out sheet.html`",
            a.sub.as_str()
        ));
    }
    if a.sub != ReadSub::Show && a.metrics_list {
        return Err(format!(
            "--metrics-list does not apply to `backtest {}` — it prints the METRIC CATALOG (every \
             id this tree can measure, with what each one means), which is `show`'s answer to \
             \"what can I ask for\" beside its --metrics. It names no run and reads no runs \
             directory: `backtest show --metrics-list`",
            a.sub.as_str()
        ));
    }
    // ⚠ `--trades` is the FIRST flag TWO sub-verbs own (`--script` below is the second), so it is
    // refused on every other reading verb rather than living in either roster: `show --trades`
    // renders the ledger and `diff --trades` compares two of them, which is the same document
    // answering the same question from two sides.
    if !matches!(a.sub, ReadSub::Show | ReadSub::Diff) && a.trades {
        return Err(format!(
            "--trades does not apply to `backtest {}` — it names the stored trade ledger, which \
             `show` renders and `diff` compares",
            a.sub.as_str()
        ));
    }
    if a.sub != ReadSub::Tag {
        refuse(
            &tag_only,
            "that flag WRITES a label, a note or a mark onto a stored run, which is what `tag` does",
        )?;
    }
    if a.sub != ReadSub::Gate {
        refuse(
            &gate_only,
            "that flag names the BASELINE or the criteria a verdict is computed from, and `gate` is \
             the verb whose product is that verdict",
        )?;
    }
    if a.sub != ReadSub::Diff {
        refuse(&diff_only, "that flag shapes a two-run COMPARISON, which is what `diff` renders")?;
    }
    if a.sub != ReadSub::Params {
        refuse(&params_only, "that flag names the built-in strategy whose keys `params` lists")?;
    }
    // ⚠ `--script` is the SECOND flag two sub-verbs own, so it gets a named rule rather than a
    // roster row: `params` lists what a script DECLARES and `script-check` compiles it, which is
    // the same file answering two questions. Refusing it on `script-check` with the `params`
    // sentence would have sent an operator to the wrong verb — the exact cost
    // `crate::cmd::data`'s `refuse_foreign_flags` argues a named refusal exists to avoid.
    if !matches!(a.sub, ReadSub::Params | ReadSub::ScriptCheck) && a.script.is_some() {
        return Err(format!(
            "--script does not apply to `backtest {}` — it names a .rhai FILE, whose knobs \
             `params` lists and which `script-check` compiles",
            a.sub.as_str()
        ));
    }
    if a.sub != ReadSub::Templates {
        refuse(
            &templates_only,
            "that flag SAVES a shipped starter to a new file, and `templates` is the verb that \
             emits one",
        )?;
    }
    if a.sub != ReadSub::Strategies {
        refuse(
            &strategies_only,
            "that flag names the COMPUTE DAEMON, and `strategies` is the one reading verb that \
             dials one — every other reads the run directory on this machine",
        )?;
    }
    // `--out` belongs to the two verbs that emit a RENDERED DOCUMENT nobody would want interleaved
    // with a terminal. `path` prints one line and `strategies` prints a roster; redirecting either
    // is the shell's job. ⚠ `gate` and `diff` are deliberately NOT here either: a gate's verdict is
    // what a CI step reads on stdout beside the rung, and both are already one clean stream a shell
    // redirect handles — the flag would be a second way to do what `>` does.
    // ⚠ `templates` is the one verb that WRITES and is still refused this flag, because its write
    // is not a rendering: `--write-strategy` refuses an existing path while `--out` overwrites, and
    // `ReadArgs::write_strategy` argues why one flag may not carry both policies.
    if !matches!(a.sub, ReadSub::Ls | ReadSub::Show) && a.out.is_some() {
        // ⚠ `templates` gets a POINTER rather than the bare sentence, because an operator typing
        // `--out` there is trying to SAVE a starter and the generic answer ("the shell can
        // redirect") would send them at a `>` that clobbers the strategy they are editing. Every
        // other sub-verb genuinely has nothing but a redirect, so it keeps the plain message.
        let hint = if a.sub == ReadSub::Templates {
            " — a starter is SAVED with --write-strategy, which refuses an existing path rather \
             than overwriting one"
        } else {
            ""
        };
        return Err(format!(
            "--out names a FILE to write a rendered document to, and `backtest {}` prints a line \
             the shell can already redirect{hint}",
            a.sub.as_str()
        ));
    }
    Ok(())
}

/// ⚠ **The return is a RUNG, not `()`, and TWO sub-verbs need that.** `gate`'s product IS its exit
/// code: it evaluates every criterion, prints the whole verdict to stdout, and the number beside it
/// is that document's summary (`crate::exit::Exit::Breach`). Routing a breach through `CliError`
/// instead would print one stderr line and throw the document away — §7.1 requires the opposite.
/// `script-check` is the second and the same shape: its product is a compile diagnostic plus a
/// number a save hook branches on, so it too prints and returns rather than erroring (it lands on
/// `crate::exit::Exit::Failed`, and `crate::cmd::strategies`'s `run_script_check` argues why not
/// `Usage` and why not `Breach`). Every other arm answers `Exit::Ok` and is unchanged.
fn execute_read(
    a: &ReadArgs,
    ctx: &crate::cmd::runs::Ctx<'_>,
    now: i64,
) -> CmdResult<crate::exit::Exit> {
    let ok = |r: CmdResult<()>| r.map(|()| crate::exit::Exit::Ok);
    match a.sub {
        ReadSub::Ls => ok(crate::cmd::runs::ls::run_ls(ctx, a)),
        ReadSub::Show => ok(crate::cmd::runs::show::run_show(ctx, a)),
        ReadSub::Path => ok(crate::cmd::runs::path::run_path(
            ctx,
            a.selector.as_deref().unwrap_or_default(),
            a.file.as_deref(),
        )),
        ReadSub::Tag => ok(crate::cmd::runs::tag::run_tag(ctx, a, now)),
        ReadSub::Diff => ok(crate::cmd::runs::diff::run_diff(ctx, a)),
        ReadSub::Gate => crate::cmd::runs::gate::run_gate(ctx, a),
        ReadSub::Params => {
            ok(crate::cmd::params::run_params(a.script.as_deref(), a.strategy.as_deref(), a.json))
        }
        ReadSub::Strategies => {
            ok(crate::cmd::strategies::run_strategies(ctx, a.addr.as_deref(), a.json))
        }
        ReadSub::Templates => ok(crate::cmd::strategies::run_templates(
            a.selector.as_deref(),
            a.write_strategy.as_deref(),
            a.json,
        )),
        ReadSub::ScriptApi => ok(crate::cmd::strategies::run_script_api(a.json)),
        // ⚠ The SECOND arm that returns a rung rather than `()`. A script that does not compile is
        // an ANSWER — the diagnostic goes to stdout in full, `--json` document included — and the
        // number beside it is what a save hook or a pre-commit branches on.
        // `crate::cmd::strategies`'s `run_script_check` argues which rung and why not the other two.
        ReadSub::ScriptCheck => {
            crate::cmd::strategies::run_script_check(a.script.as_deref(), a.json)
        }
    }
}

/// The top-level keys `BacktestProfile` accepts, as the CLI must know them — SEVEN sections, eight
/// spellings, because the parameter-search one has a permanent alias (see the ⚠ below).
///
/// ⚠ A COPY, deliberately. `crates/vike-cli/Cargo.toml` states that this crate links no
/// vike-backtest and no engine crate, so `BacktestProfile` is not nameable here at all. What makes
/// a copy acceptable is the GATE: `crates/vike-cli/tests/backtest_flags_schema.rs`'s
/// `every_cli_top_level_key_is_one_the_profile_declares` puts each of these through the real loader
/// from the test tree, where the dev-dependency reaches.
///
/// ⚠ It is the FIRST SEGMENT roster and nothing more. A full key roster would be ~90 `engine.*`
/// names plus every nested table's fields, against a schema that grew by sixteen fields in one PR
/// (#1769) — a copy that size rots between merges, and its refusal would be strictly worse than
/// serde's, which names the valid set. The top level is `deny_unknown_fields` and moves roughly
/// never, so a refusal here can never be a false one, and it catches the commonest typo class
/// (`--set egnine.fee_rate=…`) before any dial.
///
/// `base_dir` is deliberately absent: it is `#[serde(skip)]` and is not a TOML key at all.
///
/// ⚠ **The parameter-search row is `paramscan` since stage 7, and `sweep` is still accepted.**
/// Owner ruling R2 renamed the SECTION `[sweep]` → `[paramscan]` (measured: one of nineteen
/// competitor CLIs says "sweep"; QuantRocket says `paramscan`), and the parser landed it as a
/// PERMANENT serde ALIAS rather than a replacement — `vike_backtest::harness::BacktestProfile`'s
/// field is `paramscan` with `#[serde(alias = "sweep")]` — so every profile already on disk keeps
/// loading forever. BOTH rows are here because both reach the loader, and
/// `every_cli_top_level_key_is_one_the_profile_declares` puts each through the real parser: an
/// alias this array omitted would make `--set sweep.fast=…` a CLI usage error against a key the
/// engine accepts. ⚠ This doc previously said the Rust field "stays `sweep`" and that the rename
/// was a TOML-section one only; stage 7 widened it (`is_paramscan`, and the wire's own Rust names),
/// and the claim is kept beside its correction rather than deleted.
pub const PROFILE_TOP_LEVEL_KEYS: [&str; 8] =
    ["name", "data", "engine", "strategy", "risk", "paramscan", "sweep", "walkforward"];

/// How a sugar flag's raw text becomes a `toml::Value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Through [`parse_scalar`], keeping the value's TOML type. Byte-identical to the `--set`
    /// spelling by construction — which is what spec §15.2's gate asserts.
    Scalar,
    /// As a `toml::Value::String`, verbatim.
    ///
    /// ⚠ REQUIRED, not a preference. `DataCfg::from` and `DataCfg::to` are declared `String`, so a
    /// bare epoch-ms (`--from 0`) typed by [`parse_scalar`] would land as an INTEGER and be a
    /// serde type error. The residual is stated rather than implied: `--from 0` works while
    /// `--set data.from=0` does not, so these flags are not pure sugar for their `--set` spelling
    /// on a value that is itself a TOML scalar. That divergence is an argument FOR the flag.
    Str,
    /// Comma-split into an array of strings. `data.symbols` is `Vec<String>`.
    StrList,
}

/// One named sugar flag and the profile key it resolves to.
#[derive(Debug, Clone, Copy)]
pub struct Sugar {
    pub flag: &'static str,
    pub key: &'static str,
    pub shape: Shape,
}

/// Spec §5.2's selection surface — *"the minimum that must work with no file"* — as a table rather
/// than ten hand-written parse arms.
///
/// ⚠ **They are sugar, not a second path** (§5.2): each resolves to the same key its `--set`
/// spelling would, and reaches the same [`set_profile_key`] call, so the two cannot disagree about
/// WHERE a value lands. They can disagree about its TYPE — see [`Shape::Str`].
///
/// ⚠ `--align` from §5.2 is deliberately ABSENT. `DataCfg` declares no `align` field and no
/// alignment code exists: stage 0 shipped `strict` as `refuse_ragged_series` returning a named
/// `HarnessError::Data`, and `ffill`/`intersect` would be new engine-lane work in
/// `crates/vike-backtest/src/harness/run.rs`'s `load_profile_bars`, not a flag. §9.2 is half
/// closed; this surface does not pretend the other half is a command-line problem.
///
/// ⚠ `--kind` is ABSENT from §5.2's table and present here, because `DataCfg::kind` is the one
/// `[data]` field with no serde default: a profile built from §5.2's flags alone cannot parse.
///
/// Every row is gated against the real loader by
/// `crates/vike-cli/tests/backtest_flags_schema.rs`'s
/// `every_sugar_key_is_accepted_by_the_profile_loader_at_the_shape_it_writes`, which iterates THIS
/// array — so a flag added outside it would be ungated. Widen the length; do not add a sibling
/// const.
///
/// ⚠ **`--decide` is the eleventh row and the first that is not a §5.2 SELECTION knob.** It writes
/// `engine.decide`, the cross-section mode `vike_sim::DecideMode` resolves, and it is here
/// rather than as a bespoke arm for the reason this table exists: the flag and its `--set
/// engine.decide=…` spelling then reach the same key through the same applier, and the schema gate
/// above covers it by construction. Its VALUE is forwarded UNVALIDATED — see the `Shape::Str` arm
/// in [`parse_run_args`], which checks the only two rosters this side can.
pub const SUGAR: [Sugar; 11] = [
    Sugar { flag: "--venue", key: "data.venue", shape: Shape::Str },
    Sugar { flag: "--symbol", key: "data.symbols", shape: Shape::StrList },
    Sugar { flag: "--interval", key: "data.interval", shape: Shape::Str },
    Sugar { flag: "--from", key: "data.from", shape: Shape::Str },
    Sugar { flag: "--to", key: "data.to", shape: Shape::Str },
    Sugar { flag: "--kind", key: "data.kind", shape: Shape::Str },
    Sugar { flag: "--strategy", key: "strategy.name", shape: Shape::Str },
    Sugar { flag: "--cash", key: "engine.cash", shape: Shape::Scalar },
    Sugar { flag: "--fee", key: "engine.fee_rate", shape: Shape::Scalar },
    Sugar { flag: "--slippage", key: "engine.slippage", shape: Shape::Scalar },
    // ⚠ `Shape::Str`, and REQUIRED to be: `EngineCfg::decide` is declared `Option<String>`, so a
    // value typed through `parse_scalar` would land as whatever TOML type it looked like. The two
    // spellings are lowercase words, so nothing here would change today — which is exactly the
    // reason to declare the shape from the SCHEMA rather than from the values.
    Sugar { flag: "--decide", key: "engine.decide", shape: Shape::Str },
];

/// The two spellings `DataKind` accepts, for the local `--kind` roster — the
/// spelling-check-against-a-roster shape [`one_of`] applies, never a second implementation of what
/// the value MEANS.
///
/// ⚠ This read *"the same shape [`one_of`] already applies to `--rank-by` and `--optimizer`"*, and
/// those two moved to `vike_datahub_client::flag_vocab` with their match rule. So this is the only
/// roster `one_of` still checks, and the only one whose comparison is EXACT on purpose — that
/// function's doc argues why, and it is a fact about where `--kind`'s value GOES rather than a
/// leftover.
const DATA_KINDS: [&str; 2] = ["bar", "tick"];

/// The two values THIS side supplies because the schema declares no default and the flag surface
/// has no other way to say them. Applied LAST and only where nothing else set the key
/// ([`Origin::Implied`]).
///
/// * **`data.kind = "bar"`** — `DataCfg::kind` has no `#[serde(default)]`, and spec §5.2's flag
///   table has no `--kind` at all, so following it literally produces a profile that cannot parse.
///   `bar` is what a no-file run means in every shipped profile but the tick ones. ⚠ The cost is
///   that forgetting `--kind tick` silently produces a bar run — mitigated because every tick-only
///   knob is a NAMED far-side refusal (`engine.feed_latency is tick-mode only: …`), so the mistake
///   is loud exactly where it changes an answer.
/// * **`strategy.name = "rhai"` under `--script`** — [`inject_script_src`] writes
///   `[strategy.params].src` and nothing else, and
///   `crates/vike-backtest/src/harness/registry.rs`'s `"rhai"` match arm is that key's only reader.
///   `"rhai"` is deliberately absent from `STRATEGIES`, so a flags-only `--script` with no strategy
///   name produces a profile whose `src` nothing reads.
///
/// ⚠ **`engine.cash` is deliberately NOT here.** A starting balance is a modelling input with no
/// defensible default; an omitted `--cash` is a far-side missing-field refusal naming `engine.cash`,
/// which is the right answer. The asymmetry is the point: `data.kind` has an answer the schema
/// simply never wrote down, `engine.cash` does not.
fn implied_defaults(script: bool) -> Vec<Override> {
    let mut out = vec![Override {
        key: "data.kind".to_string(),
        value: toml::Value::String("bar".to_string()),
        origin: Origin::Implied("no --kind and no profile said otherwise"),
    }];
    if script {
        out.push(Override {
            key: "strategy.name".to_string(),
            value: toml::Value::String("rhai".to_string()),
            origin: Origin::Implied("--script injects a Rhai source, which only `rhai` reads"),
        });
    }
    out
}

/// Where an override came from — the precedence class [`build_profile_toml`] applies it in, and the
/// origin column `--show-effective` renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `--set key=value`, verbatim from argv.
    Set,
    /// A named sugar flag that resolves to this key (`--fee` → `engine.fee_rate`). Carries the flag
    /// so a rendered origin says which one. Applied AFTER every [`Origin::Set`], per spec §5.3's
    /// `… → --set → named sugar flag` order.
    Sugar(&'static str),
    /// Supplied by THIS side because the schema declares no default and the flag surface has no
    /// other way to say it. Applied LAST and only where nothing else set the key. Carries the
    /// reason. See `implied_defaults`.
    Implied(&'static str),
}

/// One override the command line declared, already typed and already resolved to the dotted profile
/// key it lands on.
#[derive(Debug, Clone, PartialEq)]
pub struct Override {
    pub key: String,
    pub value: toml::Value,
    pub origin: Origin,
}

/// The flags `backtest params` REFUSES BY NAME rather than silently drops.
///
/// That sub-verb reads a Rhai script on this machine and prints the knobs it declares: no server,
/// no store, no engine, and no profile to build either. A flag it accepted would have configured
/// nothing, and the operator would believe otherwise.
///
/// ⚠ They are refused BY NAME rather than falling into the unknown-argument arm, and that is not
/// decoration: every one of these EXISTS on this sub-verb's sibling `run`, so "unknown argument"
/// would tell an operator the flag does not exist — which is false, and sends them looking in the
/// wrong place. `crate::cmd::data`'s `refuse_foreign_flags` argues the same rule one shape over.
///
/// ⚠ It is a CONST rather than a literal inside the loop because the refusal and its test used to
/// be two separate literal arrays, and stage 2 added fourteen flags to the surface. Two lists that
/// must grow together fourteen times is how one of them ends up short. [`parse_read`] drives its
/// refusal from this through [`refuse_a_run_flag_on_params`], and
/// `params_refuses_every_flag_on_the_roster` iterates the same const.
///
/// ⚠ **`--profile` and `--preset` JOINED this roster in stage 3 and were deliberately absent
/// before it.** Under `--list-params` they were excused because a run and a discovery over the
/// SAME command line had to stay spellable. Under a sub-verb that argument is gone — the two are
/// no longer the same command line — so refusing them is the correct tightening.
///
/// ⚠ **Three rows LEFT it when stages 3 and 4 were merged, and each left because the surviving
/// `params` accepts the flag.** Stage 3 and stage 4 built this sub-verb in parallel and disagreed
/// about its surface: stage 3's took `--script` alone, stage 4's took `--script | --strategy` and
/// rendered a `--json` document (`crate::cmd::params`, whose module doc argues the two sources).
/// Stage 4's is the one that shipped — it is the richer verb and it carries the `SCRIPT_ONLY` fix
/// a review round found — so `--strategy` and `--json` are now VALID here and cannot be refused.
/// `--addr` left for a different reason: it is still refused on `params`, by
/// [`refuse_foreign_read_flags`]'s `strategies_only` roster, whose message names the one reading
/// verb that dials a daemon. A flag refused twice with two messages is a flag whose two refusals
/// can disagree.
const PARAMS_REFUSED: &[&str] = &[
    "--profile",
    "--preset",
    "--local",
    "--set",
    "--engine",
    "--rank-by",
    "--optimizer",
    "--euler-depth",
    "--trials",
    "--seed",
    // The §5.2 selection flags: a discovery builds no profile at all, so a selection it accepted
    // would have described a run that never happened. ⚠ `--strategy` is NOT among them any more —
    // on this sub-verb it names the ROSTER ENTRY whose knobs are being listed, not a run's
    // strategy.
    "--venue",
    "--symbol",
    "--interval",
    "--from",
    "--to",
    "--kind",
    "--cash",
    "--fee",
    "--slippage",
    // ⚠ Not a §5.2 selection flag but refused for the identical reason: it writes `engine.decide`
    // into a profile, and `params` builds no profile at all.
    "--decide",
    "--param",
    "--write-profile",
    "--show-effective",
    "--explain-data",
    "--require-coverage",
    "--max-gap",
    "--on-gap",
    "--universe",
];

/// The refusal [`parse_read`] hands back for a run-only flag typed on `params`.
///
/// ⚠ It is a FUNCTION rather than an arm written inline because stage 3 shipped it inside a
/// `parse_params_args` that no longer exists — that sub-verb's parser is [`parse_read`] now, one
/// loop shared with the other seven reading verbs — and the sentence is the part worth keeping:
/// every flag on [`PARAMS_REFUSED`] EXISTS on this sub-verb's sibling `run`, so "unknown option"
/// would tell an operator the flag does not exist, which is false and sends them looking in the
/// wrong place.
fn refuse_a_run_flag_on_params(flag: &str) -> String {
    format!(
        "{flag} does not apply to `params`, which reads the script or the strategy roster on this \
         machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli \
         backtest run`"
    )
}

/// Hand-rolled tiny arg parser (no `clap` — PR-1 adds no dependency), over the shared
/// [`crate::cmd::args`] glue: both `--flag value` and `--flag=value`. `--profile` is OPTIONAL since
/// stage 2 — flags BUILD a profile — but a command line carrying neither it nor anything to build
/// one from is refused;
/// `--addr` is left UNRESOLVED for [`resolve_addr`]; `--json` is a bare boolean. A
/// `--help`/`-h` short-circuits out through the `Err` channel; [`args::exit_for_parse_error`] is
/// what turns that back into a SUCCESS with the usage on stdout.
///
/// ⚠ It parses the tail AFTER [`claim_subcommand`] has taken the `run` token, so the first thing
/// it sees is a flag.
fn parse_run_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut profile_path: Option<String> = None;
    let mut preset_path: Option<String> = None;
    let mut script_path: Option<String> = None;
    let mut addr: Option<String> = None;
    let mut json = false;
    let mut local = false;
    let mut engine: Option<String> = None;
    let mut rank_by: Option<String> = None;
    let mut optimizer: Option<String> = None;
    let mut euler_depth: Option<String> = None;
    let mut trials: Option<String> = None;
    let mut seed: Option<String> = None;
    let mut overrides: Vec<Override> = Vec::new();
    let mut symbols: Vec<String> = Vec::new();
    let mut write_profile: Option<String> = None;
    let mut show_effective = false;

    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--profile" => profile_path = Some(flags.value(&flag, inline)?),
            "--preset" => preset_path = Some(flags.value(&flag, inline)?),
            "--script" => script_path = Some(flags.value(&flag, inline)?),
            "--addr" => addr = Some(flags.value(&flag, inline)?),
            // ⚠ REFUSED BY NAME since 2026-09-25, on BOTH arms and before `--local` is consulted, with
            // its replacement named: every history read goes through a datahub (decision 0084), so
            // the directory this flag used to name is served by a key-less one started beside the
            // run. Never "unknown argument" — somebody who typed it believes a directory will be
            // read. `crate::surface`'s `--store` row publishes this sentence verbatim.
            "--store" => return Err(flag_vocab::store_flag_removed("backtest run")),
            "--engine" => engine = Some(flags.value(&flag, inline)?),
            // `--set dotted.key=value`, REPEATABLE — the universal channel (spec decision 6: ~90
            // engine knobs would otherwise want ~90 flags).
            //
            // ⚠ The value may contain its own `=`. `Flags::next_flag` splits the ARGUMENT on the
            // first `=` only, and `Flags::value` consumes a next-argv-token value RAW without
            // re-splitting, so both spellings hand this arm one `key=value` string — which is
            // split here with `split_once`, first `=` again, for the same reason.
            //
            // ⚠ Only the FIRST segment is checked (see `PROFILE_TOP_LEVEL_KEYS`). A full-key schema
            // check is impossible from this crate and the far side's `deny_unknown_fields` names
            // the valid set better than a local roster could — at the cost of one round trip and
            // exit rung 1 rather than 2.
            //
            // ⚠ `strategy.params.*` and `sweep.*` accept ANY key SILENTLY: their serde types are
            // `toml::Value` and `Option<toml::Table>`. A typo there is a genuine no-op that no
            // schema can catch, MEASURED by `crates/vike-cli/tests/backtest_flags_schema.rs`'s
            // `the_untyped_subtrees_accept_any_key`.
            "--set" => {
                let pair = flags.value(&flag, inline)?;
                let (key, raw) = pair.split_once('=').ok_or_else(|| {
                    format!(
                        "--set takes key=value, got {pair:?} (e.g. --set engine.fee_rate=0.001)"
                    )
                })?;
                let head = key.split('.').next().unwrap_or("");
                if !PROFILE_TOP_LEVEL_KEYS.contains(&head) {
                    return Err(format!(
                        "--set {key}: a profile has no `{head}` table — the top-level keys are {}",
                        PROFILE_TOP_LEVEL_KEYS.join(", ")
                    ));
                }
                overrides.push(Override {
                    key: key.to_string(),
                    value: parse_scalar(raw),
                    origin: Origin::Set,
                });
            }
            // The §5.2 selection sugar, driven from `SUGAR` so a flag cannot exist without a key.
            //
            // ⚠ `--symbol` is the one row that does NOT push here: it is repeatable and
            // comma-splitting, and accumulates into `symbols` for a SINGLE override pushed after
            // the loop. Every occurrence therefore lands at one position in the Sugar pass, which
            // is harmless — there is exactly one `data.symbols` entry.
            f if SUGAR.iter().any(|s| s.flag == f) => {
                let s = *SUGAR.iter().find(|s| s.flag == f).expect("just matched");
                let raw = flags.value(&flag, inline)?;
                match s.shape {
                    Shape::StrList => {
                        for part in raw.split(',') {
                            let t = part.trim();
                            if t.is_empty() {
                                return Err(format!(
                                    "--symbol takes SYM[,SYM…] with no empty element, got {raw:?}"
                                ));
                            }
                            symbols.push(t.to_string());
                        }
                    }
                    Shape::Scalar => overrides.push(Override {
                        key: s.key.to_string(),
                        value: parse_scalar(&raw),
                        origin: Origin::Sugar(s.flag),
                    }),
                    Shape::Str => {
                        let v = raw.trim().to_string();
                        // The two rosters this side CAN check, and nothing more.
                        if s.flag == "--interval" && vike_model::time::interval_ms(&v).is_none() {
                            return Err(format!(
                                "--interval {v:?} is not a valid interval — a digit count and one \
                                 unit of s|m|h|d (e.g. 30s, 5m, 1h, 1d)"
                            ));
                        }
                        if s.flag == "--kind" {
                            one_of(&flag, &v, &DATA_KINDS)?;
                        }
                        // ⚠ **`--decide` is DELIBERATELY NOT checked here, and its roster is why.**
                        // `sequential | simultaneous` has no `NAMES`-shaped home on the far side:
                        // `vike_backtest::harness::profile`'s `decide_mode` MATCHES the two strings
                        // and types them again into its own refusal, so a copy on this side would
                        // be a THIRD spelling of a roster that already has two — the
                        // more-than-one-home defect `vike_backtest::data_plan`'s `OnGap::NAMES`
                        // exists to cure, bought for a local usage error. Forwarded verbatim
                        // instead, exactly as `--strategy`, `--max-gap` and the three method knobs
                        // are: the side that RUNS owns the refusal, and `decide_mode`'s names both
                        // spellings and what an absent key means. The declared cost is one wasted
                        // spawn or round trip for a typo, and a refusal about `engine.decide`
                        // rather than about `--decide`.
                        overrides.push(Override {
                            key: s.key.to_string(),
                            value: toml::Value::String(v),
                            origin: Origin::Sugar(s.flag),
                        });
                    }
                }
            }
            // `--param k=v`, REPEATABLE — sugar for `--set strategy.params.<k>=<v>`, reaching the
            // same key through the same applier.
            //
            // ⚠ NO VALIDATION IS POSSIBLE, here or on the far side. `StrategyCfg::params` is an
            // untyped `toml::Value`, so `--param typo=1` is accepted everywhere and read by
            // nothing. Spec §5.3 claims an undeclared key is a hard load error; that is true for
            // five subtrees and false for this one, which is MEASURED by
            // `crates/vike-cli/tests/backtest_flags_schema.rs`'s `the_untyped_subtrees_accept_any_key`.
            //
            // ⚠ The RANGE forms of §5.4 (`k=lo:hi:step`, `k=[a,b]`) are REFUSED rather than taken
            // as strings. A range declares a search AXIS, and `declares_a_paramscan_grid` runs on the
            // REWRITTEN text — so a `--param` that wrote a `[sweep]` table would silently reroute
            // the request from `RunBacktest` to `RunParamscanProfile` and change the response DOCUMENT,
            // with nothing in the output saying so. That is a later stage's work; until then a
            // range that looked like it worked is the worse outcome.
            //
            // ⚠ The SECTION NAME in the refusal below is a re-key site for owner ruling R2
            // ([sweep] -> [paramscan], TOML section only — `RunParamscanProfile` and every other Rust
            // name keeps its spelling). It must name the section the LOADER accepts, because this
            // message tells an operator what to write.
            "--param" => {
                let pair = flags.value(&flag, inline)?;
                let (key, raw) = pair.split_once('=').ok_or_else(|| {
                    format!("--param takes k=v, got {pair:?} (e.g. --param size=1.5)")
                })?;
                // ⚠ The EMPTY key is refused HERE rather than left to `set_profile_key`, and the
                // reason is the message. `--param =1` builds the dotted key `strategy.params.`,
                // whose trailing empty segment that function refuses — in the `--set` GRAMMAR's
                // own words, naming a flag the operator never typed. The rung was always right;
                // the flag name was not.
                if key.trim().is_empty() {
                    return Err(format!(
                        "--param takes k=v with a non-empty key, got {pair:?} \
                         (e.g. --param size=1.5)"
                    ));
                }
                if key.contains('.') {
                    return Err(format!(
                        "--param {key}: [strategy.params] is a FLAT knob table — use \
                         --set strategy.params.{key}=… if you really mean a nested key"
                    ));
                }
                if looks_like_a_search_axis(raw) {
                    return Err(format!(
                        "--param {key}={raw}: a RANGE declares a search axis, which this command \
                         cannot build yet — declare it as a [paramscan] table in a --profile, or pass \
                         a single value. (--set strategy.params.{key}={raw} sets it as a literal \
                         value if that is what you meant.)"
                    ));
                }
                overrides.push(Override {
                    key: format!("strategy.params.{key}"),
                    value: parse_scalar(raw),
                    origin: Origin::Sugar("--param"),
                });
            }
            // The five SEARCH flags, absorbed from the deleted `vike-cli sweep` (ruling 13). The
            // two SELECTORS are spelling-checked here so a typo costs no round trip and no spawn;
            // the three method KNOBS are forwarded verbatim — see [`Args::euler_depth`].
            //
            // ⚠ **THROUGH THE SHARED VOCABULARY, NOT THROUGH [`one_of`], AND THAT FIXES A LIVE
            // REFUSAL.** `flag_vocab::accept_value` owns the roster AND the match rule for these
            // two spellings, so the client now accepts every case the engine accepts —
            // `--rank-by SHARPE` and `--optimizer TPE` were a local exit-2 here and legal values
            // one crate over, measured, for as long as this file kept its own arrays. It returns
            // the CANONICAL member, which is what both fields then forward (see
            // [`Args::optimizer`]). The tombstone above carries the rest of that history.
            //
            // ⚠ `one_of` is deliberately NOT widened to do this. It also serves `--kind`, whose
            // value is written into the profile TOML rather than canonicalised — a case-insensitive
            // `one_of` would accept `--kind BAR` here and hand the far side a `kind = "BAR"` its
            // `DataKind` deserializer refuses, i.e. trade a local usage error for a remote one.
            // Widening belongs per flag, which is exactly what a vocabulary keyed on the flag does.
            "--rank-by" => {
                rank_by = Some(flag_vocab::accept_value(&flag, &flags.value(&flag, inline)?)?)
            }
            "--optimizer" => {
                optimizer = Some(flag_vocab::accept_value(&flag, &flags.value(&flag, inline)?)?)
            }
            "--euler-depth" => euler_depth = Some(flags.value(&flag, inline)?),
            "--trials" => trials = Some(flags.value(&flag, inline)?),
            "--seed" => seed = Some(flags.value(&flag, inline)?),
            // ⚠ `--search` is RETIRED on the engine and refused there by name
            // (`crates/vike-backtest/src/backtest_cli.rs`'s `parse_search_flags` carries the
            // argument for why an alias could not be made safe). It never existed on this verb, so
            // it falls into the unknown-argument arm below — which names it, which is the same
            // product.
            "--local" => {
                args::no_value(&flag, inline)?;
                local = true;
            }
            "--json" => {
                args::no_value(&flag, inline)?;
                json = true;
            }
            // ⚠ Answered with its replacement rather than called unknown. It was a MODE FLAG on
            // this verb for months, and [`claim_subcommand`] answers the bare
            // `vike-cli backtest --list-params` spelling the same way one level up.
            "--list-params" => return Err(LIST_PARAMS_RETIRED.to_string()),
            "--write-profile" => write_profile = Some(flags.value(&flag, inline)?),
            "--show-effective" => {
                args::no_value(&flag, inline)?;
                show_effective = true;
            }
            // ── the data plane's five, every one of them sugar for a `[data]` key ────────────────
            //
            // Each arm pushes an `Override` built by the data module rather than assembling one
            // here, and the reason is not tidiness: the VALUE GRAMMAR of `--on-gap` and
            // `--universe` is the roster the profile loader itself checks, so the check and the key
            // it writes belong in one place. A second spelling here is how the two drift.
            "--explain-data" => {
                args::no_value(&flag, inline)?;
                overrides.push(crate::cmd::data::explain_data_override());
            }
            "--require-coverage" => {
                args::no_value(&flag, inline)?;
                overrides.push(crate::cmd::data::require_coverage_override());
            }
            "--max-gap" => {
                overrides.push(crate::cmd::data::max_gap_override(&flags.value(&flag, inline)?)?);
            }
            "--on-gap" => {
                overrides.push(crate::cmd::data::on_gap_override(&flags.value(&flag, inline)?)?);
            }
            "--universe" => {
                overrides.push(crate::cmd::data::universe_override(&flags.value(&flag, inline)?)?);
            }
            "-h" | "--help" => return args::help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    // ⚠ `--symbol` is REPEATABLE and comma-splitting, so every occurrence folds into ONE
    // `data.symbols` override here rather than one per occurrence — `data.symbols` is a list, and
    // a per-occurrence override would leave only the last.
    if !symbols.is_empty() {
        overrides.push(Override {
            key: "data.symbols".to_string(),
            value: toml::Value::Array(symbols.into_iter().map(toml::Value::String).collect()),
            origin: Origin::Sugar("--symbol"),
        });
    }
    overrides.extend(implied_defaults(script_path.is_some()));

    if profile_path.is_none()
        && !overrides.iter().any(|o| !matches!(o.origin, Origin::Implied(_)))
        && preset_path.is_none()
        && script_path.is_none()
    {
        // ⚠ THE INVERSION'S ONE REMAINING REQUIREMENT (spec §5.3). `--profile` is optional: flags
        // build a profile. What cannot be run is a command line carrying neither a file nor
        // anything to build one FROM — it would ship an empty document and fail on the far side
        // after a dial, which is a worse answer than this one.
        //
        // ⚠ The predicate skips `Origin::Implied` entries: `implied_defaults` populates
        // `overrides` unconditionally, so `overrides.is_empty()` is never true and would make this
        // check dead. The question is whether the operator asked for anything, not whether the
        // vector has rows in it.
        //
        // ⚠ This list is the profile-BUILDING inputs and nothing else. `--json`, `--addr`,
        // `--local` and the search knobs configure a run; they do not describe one.
        return Err("nothing to run: pass --profile <run.toml>, or build one from flags\n       \
             e.g. --venue binance --symbol BTCUSDT --from 2026-01-01T00 --to 2026-02-01T00 \
             --strategy buy_hold --cash 10000\n       any profile key is reachable as \
             --set <table>.<key>=<value>"
            .to_string());
    }

    // ⚠ The two RUN MODES are exclusive, and each refuses the other's exclusive flags rather than
    // ignoring them. An `--engine` that reached a remote run would name a binary on the wrong
    // machine; an `--addr` typed beside `--local` says the operator believes they are talking to a
    // server. Silently dropping either is how somebody comes to believe a run used a store, or a
    // host, that it never touched.
    if local {
        if addr.is_some() {
            return Err(
                "--addr names a remote backtest daemon, so it cannot be combined with --local\n\
                        drop one: --local runs the engine on this machine, --addr ships the \
                        profile to a server"
                    .to_string(),
            );
        }
    } else if engine.is_some() {
        // `--store` sat beside it in a two-row loop until 2026-09-25; it is refused on BOTH arms now,
        // by its own parser arm, so this is the one flag left that only the local arm accepts.
        return Err(
            "--engine applies to --local only — a remote run reads the SERVER's store".to_string()
        );
    }
    // ⚠ **The search knobs used to be refused on the remote arm above, and stage 7 deleted that
    // refusal.** `Request::RunParamscanProfile` carries a `search` selector now, so `--optimizer`,
    // `--euler-depth`, `--trials`, `--seed` and `--rank-by multi` are not usage errors on the
    // remote route any more. A daemon too old to honour one is refused BY NAME, without sending,
    // by `DatahubClient::run_paramscan_profile` against
    // `vike_datahub_client::FEATURE_SEARCH_METHOD`.

    Ok(Args {
        profile_path,
        preset_path,
        script_path,
        addr,
        json,
        local,
        engine,
        rank_by,
        optimizer,
        euler_depth,
        trials,
        seed,
        overrides,
        write_profile,
        show_effective,
    })
}

/// Validate one flag's value against a roster, returning it owned.
///
/// A SPELLING check, never a second implementation: what a value MEANS is computed by
/// `vike_backtest::harness`, on whichever side runs. Catching a typo here is what makes it a local
/// usage error instead of a wasted round trip or a wasted spawn.
///
/// ⚠ **It serves `--kind` and nothing else now, and the narrowing is deliberate.** It used to
/// carry `--rank-by` and `--optimizer` too, against two local arrays; both of those go through
/// `vike_datahub_client::flag_vocab`'s `accept_value` instead, which owns their roster AND their
/// match rule and accepts every case the engine accepts. This helper's comparison stays EXACT, and
/// that is the correct rule for the one flag left: `--kind`'s value is written verbatim into the
/// profile TOML, so a `--kind BAR` accepted here would reach `DataKind`'s deserializer as a far
/// side refusal instead of a local one. A match rule is a per-flag fact — see
/// [`crate::surface::RosterRow`]'s `match_rule`, which exists because two rosters in this tree hold
/// the same spellings and disagree about what matches them.
fn one_of(flag: &str, value: &str, roster: &[&str]) -> Result<String, String> {
    if roster.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(format!("{flag} must be {}, got {value:?}", roster.join("|")))
    }
}

/// Whether `profile_toml` carries the non-empty `[sweep]` table a parameter search needs — `None`
/// when this side cannot tell and the engine's (or the server's) own parser must answer.
///
/// Mirrors `vike_backtest::harness::profile::BacktestProfile::is_paramscan` (present AND non-empty),
/// which is the predicate BOTH the engine and the compute server branch on. It is a PRESENCE check
/// over one key, not a profile parser: nothing here validates a slice, a strategy or a window.
///
/// ⚠ **It ROUTES the remote arm, and that closes a live divergence between the two modes.** The
/// engine has always branched on this predicate — `backtest sweep.toml` runs `harness::run_paramscan`
/// — so `--local` on a grid profile has always run the grid. The remote arm called
/// `DatahubClient::run_backtest` unconditionally, and the server's `run_backtest` runs
/// `harness::run_backtest`, which IGNORES the `[sweep]` table and reports ONE point. Same profile,
/// same verb, same flags: a grid on this machine and a single backtest on the server, with nothing
/// in the output saying which had happened. Routing here on the same predicate both far sides use
/// is what makes `--local` a rehearsal for `--addr` on a search, which is what this module's doc
/// already promises for `--preset` and `--script`.
///
/// ⚠ It runs on the REWRITTEN text — after `--preset` and `--script` are merged — so a profile
/// whose grid arrives through a preset routes the same way its contents say it should.
///
/// ⚠ **It reads TWO section names, and BOTH are live — permanently.** Ruling 2 renamed the profile
/// section `[sweep]` → `[paramscan]`, and stage 7 landed the parser rename as a serde ALIAS rather
/// than a replacement: `vike_backtest::harness::BacktestProfile`'s field is `paramscan` with
/// `#[serde(alias = "sweep")]`, so every profile already on an operator's disk keeps loading
/// FOREVER. A pre-parse that knew only one name would route the other spelling to `RunBacktest`,
/// which reports ONE point and never mentions the grid: this function's own divergence, re-opened
/// from the other side. The new name is read FIRST; a profile carrying BOTH is a serde
/// duplicate-field error on the far side, which is the right answer and not this side's to produce.
///
/// ⚠ **This paragraph used to say the opposite, and the reversal is the point of stage 7.** It read:
/// *"Accepting `paramscan` HERE costs nothing today and is not an advertisement: no shipped profile
/// carries it, `BacktestProfile` is `deny_unknown_fields` and declares only `sweep` … That is why
/// the USAGE text and every operator-facing message in this crate still say `[sweep]`."* That was
/// true and is not: the engine declares `paramscan` now, so naming it is no longer positive
/// confirmation of something false, and every operator-facing string in this crate says
/// `[paramscan]`.
///
/// ⚠ The IDENTIFIER moved with the section — stage 7's rename task renamed this function from
/// `declares_a_sweep_grid`, and `crates/vike-cli/CLAUDE.md` and
/// `crates/vike-cli/tests/search_walkforward_cli.rs`, which both cite it by name, moved with it.
/// What a rename may NOT touch is anything a PEER compares literally: the wire tags (pinned by
/// `#[serde(rename = "RunSweep")]` and its siblings), the `"sweep"` field key, the capability
/// strings `run_sweep_profile`/`run_sweep`, the MCP tool name `run_sweep`, and the `[sweep]` alias
/// this function reads above. A doc sentence claiming an identifier did NOT move is worth nothing
/// as a guard — the pass that moves the identifier rewrites the sentence too, which is exactly what
/// happened to the paragraph that used to sit here.
fn declares_a_paramscan_grid(profile_toml: &str) -> Option<bool> {
    let doc: toml::Value = toml::from_str(profile_toml).ok()?;
    match doc.get("paramscan").or_else(|| doc.get("sweep")) {
        None => Some(false),
        Some(toml::Value::Table(t)) => Some(!t.is_empty()),
        // Present and the wrong SHAPE. The engine refuses it with a type error naming the field,
        // which is a better message than anything this side could produce.
        Some(_) => None,
    }
}

/// Which wire verb carries the run. See [`route_of`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// `[walkforward]` present — the out-of-sample walk, over `RunWalkforwardProfile`. It carries
    /// a grid-bearing profile too: the window is a MODIFIER, not an alternative to searching.
    Walkforward,
    /// A non-empty grid section and no window — the parameter search, over `RunParamscanProfile`.
    Search,
    /// Everything else — one backtest, over `RunBacktest`.
    Single,
}

/// Which WIRE VERB carries this resolved profile — ruling 7's two axes, as one function:
///
/// ```text
///                        WHAT runs
///                        one backtest            a parameter search
///                                                (the grid section, non-empty)
/// HOW validated
///   one slice            RunBacktest             RunParamscanProfile
///   walked forward       RunWalkforwardProfile   RunWalkforwardProfile
///   ([walkforward])
/// ```
///
/// ⚠ **A walk-forward is a MODIFIER, not a third kind of run** — ruling 7, and it is why this
/// function is a two-axis map rather than §5.1's "window beats axis beats single". The two axes do
/// not compete: the grid says WHAT is computed and `[walkforward]` says HOW it is VALIDATED,
/// and a profile declaring both COMPOSES — each window re-searches the grid on its own training
/// half. `RunWalkforwardProfile` carries both walked-forward cells because the profile TOML crosses
/// the wire WHOLE and the far side's one parser reads the grid for itself; this side picks the
/// carrier and makes no claim about what runs inside it.
///
/// ⚠ **There is deliberately NO refusal here** — ruling 5. An earlier draft refused the pair unless
/// `[walkforward]` said `search = "sweep"`; ruling 4 deleted that key (the presence of the two
/// sections IS the statement) and ruling 5 made composition the answer. Do not re-introduce a
/// client-side refusal: this side sends the profile whole and guesses at nothing, which is the
/// property that makes `--local` a byte-identical rehearsal of `--addr`.
///
/// ⚠ **Declared residual, and it is a FAR-SIDE one.** Until ruling 4's deletion lands (the
/// walk-forward stage owns it, with `WalkforwardCfg`'s growth), an absent `walkforward.search`
/// still resolves to `WindowSearch::None`, whose driver arm trades `[strategy.params]` and never
/// reads the grid — so a composed profile routes correctly from here and runs UNSEARCHED there.
/// Nothing on this side can fix that without guessing, and guessing is what was withdrawn.
///
/// ⚠ **The two presence tests are DIFFERENT and the code is what decides.** A grid section
/// present-and-EMPTY is no search (`BacktestProfile::is_paramscan` is present-AND-non-empty, and
/// [`declares_a_paramscan_grid`] mirrors it). `[walkforward]` present-and-empty routes to the window
/// anyway, because `WalkforwardCfg::n_splits` carries no `#[serde(default)]` — so the far side's
/// parser answers with the error naming the missing key, which is a better message than anything
/// this side could produce. A key of the wrong SHAPE, and text this side cannot parse at all, both
/// go to the plain backtest for the same reason.
///
/// ⚠ It runs on the REWRITTEN text — after `--preset` and `--script` are merged — so a profile
/// routes on what it actually says.
fn route_of(profile_toml: &str, search_requested: bool) -> Route {
    let walked = match toml::from_str::<toml::Value>(profile_toml) {
        Ok(doc) => matches!(doc.get("walkforward"), Some(toml::Value::Table(_))),
        // Text this side cannot parse at all: the plain backtest, whose
        // `BacktestProfile::from_toml_str` produces the parse error naming the line.
        Err(_) => false,
    };
    // ⚠ **OR A WRITTEN SEARCH KNOB, and that second term is what closes the hole stage 7 would
    // otherwise have opened.** `--optimizer tpe` on a profile with NO grid used to route to
    // `RunBacktest`, whose server arm IGNORES a method — the same silent-downgrade class, one verb
    // over. It routes to the search verb now, where the server.s own "profile has no [paramscan]
    // table" refusal answers on the same rung the local engine.s pre-flight does (`vike_backtest`.s
    // `SearchFlags::requested && !profile.is_paramscan()`).
    //
    // ⚠ TWO TERMS, never a third BRANCH. Ruling R7 settles that walk-forward is a MODIFIER over a
    // run rather than a third run kind, so `--optimizer tpe` on a profile that ALSO declares
    // `[walkforward]` means *walk forward, searching inside each window* — and the walk-forward
    // arm is tested FIRST, below, so it cannot flatten into one search over the whole range.
    let searching = declares_a_paramscan_grid(profile_toml) == Some(true) || search_requested;
    match (walked, searching) {
        (true, _) => Route::Walkforward,
        (false, true) => Route::Search,
        (false, false) => Route::Single,
    }
}

/// The flags the WALK-FORWARD route cannot carry, refused BY NAME rather than dropped.
///
/// `DatahubClient::run_walkforward_profile` takes the profile TOML and NOTHING else — no ranking
/// field, no method selector — so a `--rank-by` or an `--optimizer` on this route configures
/// nothing at all. A window's own ranking is `[walkforward].rank_by`, in the profile, where the one
/// parser reads it.
///
/// ⚠ This is NOT the `--rank-by`-is-ignored-on-a-gridless-profile case. That one is the ENGINE's
/// documented behaviour on a route that genuinely carries the flag, and this module deliberately
/// does not second-guess it. Here the flag is dropped by THIS side, which is the thing this
/// module's doc says not to do: *"silently dropping either is how somebody comes to believe a run
/// used a store, or a host, that it never touched."*
///
/// ⚠ The message names the `[walkforward]` TABLE and no key inside it beyond `rank_by`, and that
/// is deliberate. Ruling 4 deletes `walkforward.search` — whether a window re-searches becomes the
/// grid's presence — but that deletion has NOT landed:
/// `vike_backtest::harness::profile::BacktestProfile::window_search` still reads the key, so a
/// message asserting the post-ruling rule would be positive confirmation of something false, and
/// one NAMING the key would go stale the day it is deleted. The table is true in both worlds.
fn refuse_a_walkforward_flag(args: &Args) -> Result<(), String> {
    for (flag, present) in
        [("--rank-by", args.rank_by.is_some()), ("--optimizer", args.optimizer.is_some())]
    {
        if present {
            return Err(format!(
                "{flag} configures nothing on a walk-forward run: the wire verb it goes over \
                 carries the profile TOML and nothing else. A window's ranking is \
                 [walkforward].rank_by, in the profile, read by the one parser that runs it — and \
                 what each window searches is that same [walkforward] table's business, never a \
                 flag on this side."
            ));
        }
    }
    Ok(())
}

/// `--local` cannot run a walk-forward, and the reason is a property of the ENGINE rather than of
/// this verb: `crates/vike-backtest/src/backtest_cli.rs` has one profile path, branching on
/// `BacktestProfile::is_paramscan` and nothing else, so NEITHER driver
/// (`vike_backtest::harness::run_walkforward` or its optimizing sibling) is reachable from any
/// binary. There is nothing to spawn. `crates/vike-cli/src/cmd/walkforward.rs`'s module doc
/// carries the condition that would change it.
const WALKFORWARD_HAS_NO_LOCAL_ARM: &str = "--local cannot run a walk-forward: this profile declares a [walkforward] table, and the \
     standalone engine has ONE profile path — it branches on the [paramscan] grid and nothing else, so \
     neither walk-forward driver is reachable from any binary and there is nothing local to \
     spawn.\n\
     Drop --local to run it on the compute daemon, or drop the [walkforward] table to backtest \
     this profile here.";

/// Print a server `ParamscanReport` as a human table — one row per grid point, in the order the SERVER
/// ranked them. Every number printed is read straight out of that row's `BacktestReport`; nothing
/// is recomputed here, which is the workspace rule that no metric is reimplemented outside
/// `vike-backtest`.
///
/// # ⚠ The FIX this function carries: `max_dd` was published a hundred times too small
///
/// It printed `num(r, "max_drawdown")` under `{:>8.4}` — a bare fraction, no `* 100.0` and no `%`
/// — under a column headed `max_dd`, **directly beside a `return` column that WAS scaled and did
/// carry a `%`**. So a three-percent drawdown read `0.0310` on the row an operator sizes risk
/// from, next to `+5.00%`: one table, two conventions, and the smaller-looking number was the one
/// that matters. That is verbatim the failure
/// `vike_analytics::metric_catalog::MetricUnit::render`'s doc names — *"a renderer that drops the
/// `* 100.0` publishes a max drawdown of three percent as `0.0310%`, which reads as three basis
/// points"* — and `max_drawdown` has carried `MetricUnit::Percent` in that catalog all along.
/// Nothing pinned the numeric cells (`crates/vike-cli/tests/search_walkforward_cli.rs`'s
/// `a_sweep_profile_routes_to_the_search_verb_and_prints_the_server_ranked_table` asserts the
/// banner, the rank name and one override), so it stayed green.
///
/// **Every cell now goes through [`cell`], i.e. through the catalog.** This function keeps the
/// COLUMN WIDTHS and nothing else about a number's appearance, which is the division
/// `MetricUnit::render`'s own doc draws: *"It also does not pad, align or label."*
///
/// ⚠ Three visible consequences, all deliberate, none of them a bug to "fix back":
///   * `return` and `max_dd` read at FOUR decimals with a `%` (the `Percent` unit's precision),
///     where `return` used to read at two.
///   * the explicit `+` on a positive return is gone; a negative one still renders its `-`.
///   * the header and the row now use IDENTICAL widths. They did not before — the header's
///     `{:>9}` for `return` was hand-matched against a row's `{:>+8.2}%`, i.e. eight plus a
///     percent sign, which is the kind of arithmetic that goes wrong the first time a width moves.
///
/// ⚠ **`show`'s and `report::stored`'s verbatim carry-through is NOT this defect and must not be
/// converted.** Those two read a stored `report.json` as `serde_json::Value` and print the
/// producer's own numbers unchanged, deliberately, because a `null` normalised to `0.0` would make
/// a broken run look like a flat one — their module docs carry that argument. This table is a
/// RENDERER of live server output with a header of its own, which is the case the catalog owns.
fn print_paramscan(report: &Value) {
    let rows = report.get("rows").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    if rows.is_empty() {
        println!("(the search returned no grid points)");
        return;
    }
    let rank_by = report.get("rank_by").and_then(Value::as_str).unwrap_or("sharpe");
    println!("parameter search — {} point(s), ranked server-side by {rank_by}", rows.len());
    // ⚠ The METHOD's own cost line, when the method had one. The engine binary prints this to its
    // own stderr; a REMOTE run has no stderr to read, so before stage 7 a euler or tpe search
    // reported its budget nowhere. The grid carries none and prints none, exactly as before.
    if let Some(summary) = report.get("summary").and_then(Value::as_str) {
        println!("{summary}");
    }
    println!(
        "{:>4}  {:<34}  {:>14}  {:>10}  {:>8}  {:>9}  {:>7}",
        "rank", "overrides", "final_equity", "return", "sharpe", "max_dd", "trades"
    );
    for (i, row) in rows.iter().enumerate() {
        let overrides = fmt_overrides(row.get("overrides"));
        match row.get("report").filter(|r| !r.is_null()) {
            Some(r) => println!(
                "{:>4}  {:<34}  {:>14}  {:>10}  {:>8}  {:>9}  {:>7}",
                i + 1,
                overrides,
                cell(r, "final_equity"),
                cell(r, "total_return"),
                cell(r, "sharpe"),
                cell(r, "max_drawdown"),
                cell(r, "n_trades"),
            ),
            // A per-point failure never fails the whole search server-side — it comes back as this
            // row's `error`, and the row still prints (which point failed, and why).
            None => println!(
                "{:>4}  {:<34}  FAILED: {}",
                i + 1,
                overrides,
                row.get("error").and_then(Value::as_str).unwrap_or("(unknown error)")
            ),
        }
    }
}

/// Render one row's `[[name, value], …]` overrides as `k=v, k=v`. The values are TOML scalars
/// serialized into JSON, so they print through [`Value`]'s own `Display` (nothing is re-typed).
fn fmt_overrides(overrides: Option<&Value>) -> String {
    let Some(pairs) = overrides.and_then(Value::as_array) else {
        return String::new();
    };
    pairs
        .iter()
        .filter_map(|p| {
            let pair = p.as_array()?;
            let (k, v) = (pair.first()?.as_str()?, pair.get(1)?);
            Some(format!("{k}={v}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One numeric field of a server `BacktestReport`; `0.0` when absent or non-numeric (a
/// `"sharpe": null` — serde_json's encoding of a NaN Sharpe over a flat curve — prints as `0.0000`
/// rather than breaking the table).
fn num(report: &Value, field: &str) -> f64 {
    report.get(field).and_then(Value::as_f64).unwrap_or(0.0)
}

/// One CATALOG metric of a server `BacktestReport`, rendered the way that metric's own unit says it
/// must be read — the scaling and the precision from
/// `vike_analytics::metric_catalog::MetricUnit::render`, which is *"the ONE home"* for both, and
/// the column width from the caller.
///
/// ⚠ **The `None` arm is unreachable for every id [`print_paramscan`] passes**, and
/// [`tests::every_metric_the_ranked_table_prints_has_a_catalog_row`] is what keeps it so — a
/// `spec_for` that could silently answer nothing is how a column starts lying again. It renders
/// through `Ratio` rather than panicking because that is the one fallback that invents nothing: no
/// scaling applied, no suffix implied, the number as it arrived at four decimals. A panic here
/// would lose the whole ranked table an operator has already waited for a search to produce.
fn cell(report: &Value, id: &str) -> String {
    let v = num(report, id);
    match vike_analytics::metric_catalog::spec_for(id) {
        Some(spec) => spec.unit.render(v),
        None => vike_analytics::metric_catalog::MetricUnit::Ratio.render(v),
    }
}

/// Read the profile, ship it to the datahub server, and print the report — pretty by default, raw
/// under `--json`. Every failure path funnels into ONE [`CliError`] the caller prints to stderr and
/// exits on; the ones that are not explicitly classified arrive through `From<String>` on the
/// pre-existing rung, which is what let this file be converted without re-judging every `?`.
fn execute(
    args: &Args,
    project_root: Option<&Path>,
    user_data_dir: Option<&Path>,
    configured_addr: Option<&str>,
    keys: Option<&NodeKeys>,
) -> CmdResult<()> {
    // THE INVERSION (spec §5.3). `--profile` is now a BASE rather than the whole input: the file's
    // text, when there is one, is the document every override is applied onto. Everything below
    // this point sees only the resulting TEXT, which is what keeps `--local` a byte-identical
    // rehearsal of `--addr`.
    let base = match args.profile_path.as_deref() {
        Some(path) => Some(
            std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read profile {path}: {e}"))?,
        ),
        None => None,
    };
    let mut profile_toml =
        build_profile_toml(base.as_deref(), &args.overrides).map_err(CliError::usage)?;

    // --preset: merge the preset file's knobs into `[strategy.params]`. BEFORE `--script`, so the
    // script's own `src` can never be shadowed by anything a params file carries (and a preset
    // carrying `src` at all is refused outright — see `merge_preset_params`).
    if let Some(preset_path) = &args.preset_path {
        let preset = std::fs::read_to_string(preset_path)
            .map_err(|e| format!("cannot read preset {preset_path}: {e}"))?;
        profile_toml = merge_preset_params(&profile_toml, &preset)
            .map_err(|e| format!("preset {preset_path}: {e}"))?;
    }

    // --script: inject the .rhai file's SOURCE into the profile's `[strategy.params].src`, so an
    // authored strategy ships self-contained — the (possibly remote) datahub server has no access
    // to this client's filesystem, only the profile text.
    if let Some(script_path) = &args.script_path {
        let src = std::fs::read_to_string(script_path)
            .map_err(|e| format!("cannot read script {script_path}: {e}"))?;
        profile_toml = inject_script_src(&profile_toml, &src)?;
    }

    // ⚠ BELOW both rewrites, deliberately: what `--write-profile` writes and what `--show-effective`
    // prints must be the text that would ACTUALLY have been run, `--preset`'s merge and
    // `--script`'s injected source included. Writing the pre-rewrite document would produce a file
    // that runs differently from the command line that produced it, which is the one thing spec
    // §15.4's round-trip gate exists to prevent.
    if let Some(path) = &args.write_profile {
        if std::path::Path::new(path).exists() {
            return Err(CliError::usage(format!(
                "--write-profile {path} already exists, and this command will not overwrite a \
                 profile — delete it, or name a different path"
            )));
        }
        std::fs::write(path, &profile_toml)
            .map_err(|e| format!("cannot write the profile to {path}: {e}"))?;
    }
    if args.show_effective {
        print!("{}", render_effective(&profile_toml, &args.overrides));
        return Ok(());
    }

    // ⚠ THE PROFILE ROUTES, not a flag — ruling 7's two axes, folded in ONE function so the
    // carrier choice and the fall-throughs cannot come to disagree. Computed on the REWRITTEN
    // text, for the reason the `--local` branch below is placed this far down: a local run and a
    // remote run are given byte-identical profile text, so they must route identically too. It is
    // INFALLIBLE (ruling 5 withdrew its only refusal), so there is nothing to lift onto an exit
    // rung here.
    let route = route_of(&profile_toml, args.search_requested());

    if route == Route::Walkforward {
        // Refused BEFORE the `--local` branch, so a walk-forward never reaches
        // `crate::cmd::engine`'s search for a binary that could not run it anyway.
        if args.local {
            return Err(CliError::usage(WALKFORWARD_HAS_NO_LOCAL_ARM));
        }
        refuse_a_walkforward_flag(args).map_err(CliError::usage)?;
    }

    // ⚠ `--local` DIVERGES HERE, and everything above it is deliberately shared: the profile has
    // been read and both client-side rewrites have been applied, so a local run and a remote run
    // are given byte-identical profile TEXT. That is the whole point of putting the branch this
    // far down — `--preset` and `--script` are resolved against THIS filesystem in both modes, and
    // a local run that quietly resolved them differently would answer differently from the remote
    // run it is supposed to be a rehearsal for.
    if args.local {
        return execute_local(
            args,
            project_root,
            user_data_dir,
            args.profile_path.as_deref(),
            &profile_toml,
        );
    }

    // ⚠ THE one site on this path that is worth its own rung: the server was not there. Same
    // sentence as before, on the CONNECT rung — the caller that should back off and retry (or go
    // open its SSH tunnel) can now tell this apart from a profile it typed wrong.
    // ⚠ Scope::Write, not Observe: `vike_datahub_client::proto`'s `required_scope` groups every
    // profile-running verb with the WRITE and DESTRUCTIVE ones, because each COMPILES
    // client-supplied Rhai on the server. `None` keeps the unauthenticated dial a key-less
    // server has always answered.
    // ⚠ THE LADDER IS FOLDED HERE, not in the parser: `config.backtest_addr` is resolved by the
    // composition root and handed in, because a `src/cmd/` file reads no settings of its own.
    let addr = resolve_addr(args.addr.as_deref(), configured_addr);
    let mut client = match keys {
        Some(k) => DatahubClient::connect_authed(&addr, k, Scope::Write),
        None => DatahubClient::connect(&addr),
    }
    .map_err(|e| CliError::connect(format!("cannot connect to the backtest daemon at {addr}: {e} (start it with `vike-backend backtest --addr`)")))?;

    // ⚠ **THE PROFILE ROUTES, not a flag** — and this is where `vike-cli sweep` went (ruling 13:
    // there is no second verb, `--optimizer` is where the word "optimize" is spelled) and, since
    // decision 3, where `vike-cli walkforward` went too. [`route_of`] is the whole map.
    //
    // ⚠ It also closes a divergence that predates the merge. The ENGINE has always branched on
    // the grid predicate, so `--local` on a grid profile ran the grid — while this arm called
    // `run_backtest` unconditionally, and the server's `run_backtest` runs `harness::run_backtest`,
    // which ignores the `[sweep]` table and reports ONE point. Same profile, same verb, same
    // flags: a grid here and a single backtest there, with nothing in the output saying which had
    // happened. See [`declares_a_paramscan_grid`]. A `[walkforward]` table was the same defect wearing
    // a second section name, and [`route_of`] closes both.
    let report_json = match route {
        Route::Walkforward => client.run_walkforward_profile(&profile_toml)?,
        // A transport failure and a server-side `Response::Error` both arrive as `Err(String)`
        // and stay on the pre-existing rung: once the connection is open, a failure is the run's.
        Route::Search => client.run_paramscan_profile(
            &profile_toml,
            args.rank_by.as_deref(),
            args.wire_search().as_ref(),
        )?,
        Route::Single => client.run_backtest(&profile_toml)?,
    };

    if args.json {
        // Verbatim: exactly the JSON the server emitted. THREE different documents, one per
        // carrier — a `BacktestReport`, a `ParamscanReport` for a search, a `WalkForwardReport` for a
        // window — and they always were different; the route is what says which one arrived.
        println!("{report_json}");
        return Ok(());
    }
    // Re-parse to a generic `Value` so we do not need `BacktestReport` to derive `Deserialize` (it
    // does not yet — see the proto doc); a parse failure means the server sent something that is
    // not the report JSON, which is worth surfacing.
    let value: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server report was not valid JSON: {e}"))?;
    match route {
        // The stitched OOS table — one row per window, plus the `chosen` column an OPTIMIZING walk
        // earns and a fixed one does not print at all. Rendered by
        // `crate::cmd::walkforward::print_walkforward`, which stayed where it was.
        Route::Walkforward => crate::cmd::walkforward::print_walkforward(&value),
        // The ranked table, rendered from the SERVER's own per-row statistics in the SERVER's own
        // order. Absorbed from the deleted `sweep` verb unchanged — see [`print_paramscan`].
        Route::Search => print_paramscan(&value),
        Route::Single => {
            let pretty = serde_json::to_string_pretty(&value)
                .map_err(|e| format!("cannot pretty-print report: {e}"))?;
            println!("{pretty}");
        }
    }
    Ok(())
}

/// The `--local` arm: run the backtest on THIS machine by driving the standalone engine.
///
/// ⚠ **It spawns rather than links, and that is not a shortcut** — `crates/vike-cli/src/cmd/
/// engine.rs` carries the whole argument (this crate's identity is DataFusion-free, and the engine
/// opens a `DataFusionHist`), the search order, and the fold from the child's exit status onto this
/// crate's ladder.
///
/// # What is forwarded, and the one thing that is NOT
///
/// `--profile` and `--json` go to the child as themselves (`--store` did too, until it was refused on
/// both arms on 2026-09-25). `--preset` and `--script` do
/// NOT: they are CLIENT-SIDE rewrites (`merge_preset_params`, `inject_script_src`) that the engine
/// has no flags for, and asking it to grow them would put a second copy of the merge rules in the
/// tree. The rewritten profile is staged under `<project>/tmp` instead — through
/// `vike_model::scratch::ScratchDir`, which owns the directory and removes it on drop, including on
/// the panic path — and the child is handed THAT path.
///
/// So the profile the local engine parses is byte-identical to the text a remote datahub would have
/// been shipped, which is the property that makes `--local` a rehearsal for `--addr` rather than a
/// second, subtly different run mode.
///
/// # What the child is TOLD beside its argv
///
/// `user_data_dir` — the directory the reading verbs join their runs root onto — goes to the child
/// in its environment, so the run it saves is the run `backtest ls` lists. The child's own answer
/// comes from a walk that does not read `$VIKE_SETTINGS_DIR`; [`run`]'s doc carries the incident.
fn execute_local(
    args: &Args,
    project_root: Option<&Path>,
    user_data_dir: Option<&Path>,
    profile_path: Option<&str>,
    profile_toml: &str,
) -> CmdResult<()> {
    // ⚠ STAGE WHENEVER THE CHILD CANNOT BE HANDED THE OPERATOR'S OWN FILE — which, since stage 2 of
    // the backtest-CLI-surface design, is any run whose profile was BUILT rather than read, and any
    // run whose flags changed it.
    //
    // ⚠ The comparison is on TEXT, and the builder NORMALIZES (`toml::to_string` over a re-parsed
    // `toml::Value` drops comments and reorders keys). So a commented profile always compares
    // unequal and is always staged. That is correct rather than unfortunate: the child must receive
    // the same bytes the remote arm would, which is the whole reason `execute`'s `--local` branch
    // sits BELOW every rewrite. Do NOT "optimise" this by skipping the build when there are no
    // overrides — that would stop `--local` being a byte-identical rehearsal of `--addr`.
    //
    // ⚠ Held for the whole call: `ScratchDir`'s `Drop` is what removes the staged profile, so
    // binding it to `_` (rather than to a name) would delete the file before the child reads it.
    let staged;
    let profile_arg: &Path = match profile_path {
        Some(p) if std::fs::read_to_string(p).is_ok_and(|t| t == profile_toml) => Path::new(p),
        _ => {
            let root = crate::cmd::engine::scratch_root(project_root).ok_or_else(|| {
                CliError::failed(format!(
                    "--local runs the engine on this machine from a profile FILE, and there is no \
                     project above the working directory to stage one in.\n{}\nRun inside your \
                     project (or set $VIKE_SETTINGS_DIR), pass an already-merged profile to \
                     --profile, or write one first with --write-profile <path>",
                    match profile_path {
                        Some(p) => format!(
                            "The flags on this command line changed {p}, so the child cannot be \
                             handed it unchanged."
                        ),
                        None => "This run's profile was built from flags, so there is no file to \
                                 hand the child."
                            .to_string(),
                    }
                ))
            })?;
            staged = StagedProfile::write(&root, profile_toml)?;
            staged.path()
        }
    };

    let mut argv: Vec<std::ffi::OsString> = vec!["--profile".into(), profile_arg.into()];
    // ⚠ The five SEARCH flags go to the child as THEMSELVES, spelled identically — they are the
    // engine's own flags (#1750's `parse_search_flags`), so this is forwarding rather than
    // translation. Nothing here decides whether a search HAPPENS: the profile's `[sweep]` table
    // does, on both sides, which is what makes `--local` a rehearsal for `--addr`.
    //
    // ⚠ The two SELECTORS carry their CANONICAL spelling rather than the operator's typing, for
    // the reason `flag_vocab::accept_value`'s doc gives: the child gets `tpe` for a typed `TPE`,
    // which is the same byte sequence the `--addr` arm puts in a `WireSearch`. That is what keeps
    // the two routes a rehearsal of each other on a search rather than two spellings of one.
    //
    // ⚠ The three method KNOBS are forwarded unvalidated on purpose. The engine owns the ownership
    // rule (`--trials` under `--optimizer euler` is REFUSED, from a table), the ranges and the
    // caps, and each refusal names the method that owns the knob. A second copy of that table here
    // would be one more thing to keep in step and could only ever produce a worse message.
    for (flag, value) in [
        ("--rank-by", &args.rank_by),
        ("--optimizer", &args.optimizer),
        ("--euler-depth", &args.euler_depth),
        ("--trials", &args.trials),
        ("--seed", &args.seed),
    ] {
        if let Some(v) = value {
            argv.push(flag.into());
            argv.push(v.into());
        }
    }
    if args.json {
        argv.push("--json".into());
    }
    // ⚠ TOLD where `user_data` is, rather than left to find it: the child's own resolution is a walk
    // from its working directory that does not read `$VIKE_SETTINGS_DIR`, so without this a run
    // could land in a project `backtest ls` never looks in. See [`run`]'s doc and
    // `crate::cmd::engine`'s `Engine::with_user_data_dir`.
    let program = crate::cmd::engine::locate(args.engine.as_deref(), project_root)
        .with_user_data_dir(user_data_dir);
    crate::cmd::engine::run(&program, &argv, "backtest")
}

/// A profile written into `<project>/tmp` for a child process to read, removed when this value is
/// dropped.
///
/// A thin wrapper rather than a bare path because the OWNERSHIP is the point: the directory guard
/// has to outlive the child, and a function returning a `PathBuf` out of a dropped `ScratchDir`
/// would compile and then hand the engine a file that is already gone.
struct StagedProfile {
    /// The guard. Never read — its `Drop` is the whole job — and named rather than `_` so it is
    /// obvious that dropping it early is what breaks this.
    _dir: vike_model::scratch::ScratchDir,
    path: PathBuf,
}

impl StagedProfile {
    fn write(scratch_root: &Path, profile_toml: &str) -> CmdResult<Self> {
        let dir = vike_model::scratch::ScratchDir::create_in(scratch_root, "vike-cli-local")
            .map_err(|e| {
                CliError::failed(format!(
                    "cannot create a scratch directory under {}: {e}",
                    scratch_root.display()
                ))
            })?;
        let path = dir.path().join("profile.toml");
        std::fs::write(&path, profile_toml).map_err(|e| {
            CliError::failed(format!(
                "cannot stage the rewritten profile at {}: {e}",
                path.display()
            ))
        })?;
        Ok(Self { _dir: dir, path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

/// Inject an authored Rhai script's `src` into a profile's `[strategy.params].src`, returning the
/// re-serialized profile TOML to ship. Parses the profile as a `toml::Value` (a bad profile is a
/// clean error, not a panic), creates `[strategy]`/`[strategy.params]` if absent, and sets/overwrites
/// `src`. Existing params (the numeric knobs a `[sweep]` varies) are preserved. Any pre-existing
/// inline `src` is overwritten — the `--script` file wins.
pub(crate) fn inject_script_src(profile_toml: &str, script_src: &str) -> Result<String, String> {
    let mut doc: toml::Value =
        toml::from_str(profile_toml).map_err(|e| format!("profile is not valid TOML: {e}"))?;
    strategy_params_mut(&mut doc)?
        .insert(SRC_KEY.to_string(), toml::Value::String(script_src.to_string()));
    toml::to_string(&doc)
        .map_err(|e| format!("cannot re-serialize profile after --script inject: {e}"))
}

/// Merge a PRESET's knobs into a profile's `[strategy.params]`, returning the re-serialized profile
/// TOML to ship — the `--preset` half of the same client-side rewrite [`inject_script_src`] does.
///
/// A preset IS the params table: a FLAT table of a strategy's knobs, whose keys land in
/// `[strategy.params]` one for one, last-wins over anything the profile already set there. Every
/// other key of the profile is untouched.
///
/// # Two shapes are REFUSED, both because accepting them would do nothing visible
///
/// * **A `[params]` wrapper.** Merged as-is it would give the strategy one parameter called
///   `params` that no `from_params` reader looks at, while every knob silently kept its default —
///   the worst available outcome, because the user did everything else right. Silently UNWRAPPING it
///   instead would make two shapes legal and pick between them by an invisible rule.
/// * **A `src` key**, which is the strategy's SOURCE rather than a knob. Allowing it would let a
///   params file smuggle a whole script past `--script`, and two overwrite rules interacting is
///   exactly how a silent surprise gets built.
///
/// ⚠ **This rule is stated in two crates and that is deliberate.**
/// `crates/vike-studio-core/src/user_strategies/load.rs`'s `check_preset_shape` is the authority and
/// carries the full argument; this CLI cannot call it, because `vike-studio-core` depends on
/// `vike-data/hist-datafusion` and this crate's whole identity is being DataFusion-free on the fast
/// lane (see the `[dependencies]` rationale in `crates/vike-cli/Cargo.toml`). The rule is ten lines
/// and the alternative is dragging Arrow into a laptop binary.
pub(crate) fn merge_preset_params(profile_toml: &str, preset_toml: &str) -> Result<String, String> {
    let preset: toml::Value =
        toml::from_str(preset_toml).map_err(|e| format!("not valid TOML: {e}"))?;
    let knobs = preset.as_table().ok_or("a preset must be a table of parameters")?;
    if knobs.len() == 1 && knobs.get(PARAMS_WRAPPER_KEY).is_some_and(toml::Value::is_table) {
        return Err(format!(
            "it wraps its knobs in a [{PARAMS_WRAPPER_KEY}] table, so the strategy would receive \
             one parameter called '{PARAMS_WRAPPER_KEY}' that nothing reads and every knob would \
             keep its default. A preset IS the params table: delete the [{PARAMS_WRAPPER_KEY}] \
             header and leave the keys at the top level"
        ));
    }
    if knobs.contains_key(SRC_KEY) {
        return Err(format!(
            "it defines '{SRC_KEY}', which is the strategy's SOURCE rather than one of its knobs — \
             that is what `--script` is for. Delete the '{SRC_KEY}' key"
        ));
    }

    let mut doc: toml::Value =
        toml::from_str(profile_toml).map_err(|e| format!("profile is not valid TOML: {e}"))?;
    let params = strategy_params_mut(&mut doc)?;
    for (key, value) in knobs {
        params.insert(key.clone(), value.clone());
    }
    toml::to_string(&doc)
        .map_err(|e| format!("cannot re-serialize profile after --preset merge: {e}"))
}

/// The profile's `[strategy.params]` table, CREATING `[strategy]` and `[strategy.params]` when
/// absent — the one place both client-side rewrites above reach into a profile, so they cannot
/// disagree about where params live or about what a non-table there means.
fn strategy_params_mut(
    doc: &mut toml::Value,
) -> Result<&mut toml::map::Map<String, toml::Value>, String> {
    let root = doc.as_table_mut().ok_or("profile root is not a TOML table")?;
    let strategy = root
        .entry("strategy")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .ok_or("[strategy] is not a table")?;
    strategy
        .entry("params")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .ok_or_else(|| "[strategy.params] is not a table".to_string())
}

/// Type a `--set` VALUE by parsing it as a one-key TOML DOCUMENT (`v = <text>`), falling back to a
/// bare string.
///
/// ⚠ **`toml::Value` has no scalar `FromStr`.** `"2.5".parse::<toml::Value>()` is a DOCUMENT parse
/// in the `toml` crate too, so a bare `2.5` is not a valid document and the obvious implementation
/// types EVERY value as a string. `crates/vike-studio-core/src/spec.rs`'s `parse_scalar` and
/// `crates/vike-studio/src/remote.rs`'s `parse_toml_value` are the two existing implementations of
/// this idiom and both say so in their own comments; neither crate is reachable from this
/// DataFusion-free CLI, so this is a third copy —
/// `crates/vike-cli/tests/backtest_flags_schema.rs` is what holds it to the real profile loader.
///
/// ⚠ `vike_config::write::set_setting` does the same job with `trimmed.parse::<Value>()`, and that
/// is NOT a counter-example: its `Value` is `toml_edit::Value`, which DOES parse a bare scalar.
/// This crate links `toml` and not `toml_edit`, and the two are not interchangeable.
///
/// Wrongly-typed input is not this site's problem to guess at: the profile loader on the far side
/// refuses it with the key's own message.
pub fn parse_scalar(text: &str) -> toml::Value {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return toml::Value::String(String::new());
    }
    match toml::from_str::<toml::Value>(&format!("v = {trimmed}")) {
        Ok(doc) => {
            doc.get("v").cloned().unwrap_or_else(|| toml::Value::String(trimmed.to_string()))
        }
        Err(_) => toml::Value::String(trimmed.to_string()),
    }
}

/// Whether a `--param` VALUE is one of spec §5.4's search-axis spellings rather than a single value.
///
/// Two forms: a bracketed category list (`[trend,chop]`) and a `lo:hi:step` numeric triple. The
/// triple is checked by SHAPE — three colon-separated parts that all parse as `f64` — so an
/// ordinary value that merely CONTAINS a colon (`binance:BTCUSDT`) is untouched.
///
/// ⚠ The residual: a genuine string param spelled `1:2:3` has no `--param` spelling. It is
/// reachable through `--set strategy.params.<k>=1:2:3`, and the refusal names that route.
fn looks_like_a_search_axis(raw: &str) -> bool {
    let t = raw.trim();
    if t.starts_with('[') {
        return true;
    }
    let parts: Vec<&str> = t.split(':').collect();
    parts.len() == 3 && parts.iter().all(|p| p.trim().parse::<f64>().is_ok())
}

/// Assign `value` at `dotted_key` inside a parsed profile document, creating the tables the path
/// needs.
///
/// The `vike_config::write::set_setting` walk, transplanted onto `toml::Value`: split on `.`,
/// refuse an empty segment, walk the parents creating tables, refuse a path THROUGH a plain value,
/// refuse a key naming a whole table, then insert. Four refusals, each naming the key.
///
/// ⚠ A key naming a whole TABLE is refused rather than replaced. Overwriting `[engine.impact]` with
/// a scalar is never what an operator meant, and the remedy — set its leaves — is what the message
/// says. The cost is that an inline-table replacement has no `--set` spelling.
///
/// ⚠ It performs NO schema check. `BacktestProfile` is not nameable from this crate (see the
/// manifest's own comment), so an undeclared key is refused on the far side by
/// `#[serde(deny_unknown_fields)]` — serde's message names the valid set, which no local roster
/// could do as well or keep as current. The one check this side DOES make is on the FIRST segment;
/// see [`PROFILE_TOP_LEVEL_KEYS`].
pub fn set_profile_key(
    doc: &mut toml::Value,
    dotted_key: &str,
    value: toml::Value,
) -> Result<(), String> {
    let segs: Vec<&str> = dotted_key.split('.').collect();
    if segs.iter().any(|s| s.trim().is_empty()) {
        return Err(format!(
            "--set key {dotted_key:?} has an empty segment — a key is spelled `table.key` (or \
             `table.sub.key`), with no leading, trailing or doubled dot"
        ));
    }
    let mut cur =
        doc.as_table_mut().ok_or_else(|| "profile root is not a TOML table".to_string())?;
    let (leaf, parents) = segs.split_last().expect("split('.') yields at least one segment");
    for (i, seg) in parents.iter().enumerate() {
        if !cur.contains_key(*seg) {
            cur.insert((*seg).to_string(), toml::Value::Table(Default::default()));
        }
        cur = cur.get_mut(*seg).expect("present or just inserted").as_table_mut().ok_or_else(
            || {
                format!(
                    "`{}` is a plain value in the profile, so `{dotted_key}` cannot nest under it",
                    segs[..=i].join(".")
                )
            },
        )?;
    }
    if cur.get(*leaf).is_some_and(toml::Value::is_table) {
        return Err(format!(
            "`{dotted_key}` names a whole table in the profile, not a single value — set its \
             leaves instead (`{dotted_key}.<key>`)"
        ));
    }
    cur.insert((*leaf).to_string(), value);
    Ok(())
}

/// Build the profile TEXT this invocation ships: the `--profile` file (or an empty document when
/// there is none), with every override applied in spec §5.3's order.
///
/// ```text
/// schema default  →  [profile file]  →  --set  →  named sugar flag
/// ```
///
/// The passes are what make that order STRUCTURAL rather than a convention: a `Sugar` entry and a
/// `Set` entry naming the same key reach the same [`set_profile_key`] call with the same value, so
/// `--fee X` and `--set engine.fee_rate=X` cannot produce different documents (spec §15.2, gated by
/// `sugar_and_set_produce_the_same_document`). [`Origin::Implied`] is a fourth pass the spec's
/// ladder does not name, applied only where nothing else did — it exists because two keys have no
/// schema default and no other flag spelling; see `implied_defaults`.
///
/// ⚠ The output is a NORMALIZED re-serialization. `toml::to_string` over a re-parsed `toml::Value`
/// drops comments and reorders keys, exactly as [`merge_preset_params`] and [`inject_script_src`]
/// already do — so `--write-profile` round-trips the RESOLVED profile, never the operator's file
/// text (spec §15.4).
pub fn build_profile_toml(base: Option<&str>, overrides: &[Override]) -> Result<String, String> {
    let mut doc: toml::Value = match base {
        Some(text) => {
            toml::from_str(text).map_err(|e| format!("profile is not valid TOML: {e}"))?
        }
        None => toml::Value::Table(Default::default()),
    };
    for ov in overrides.iter().filter(|o| matches!(o.origin, Origin::Set)) {
        set_profile_key(&mut doc, &ov.key, ov.value.clone())?;
    }
    for ov in overrides.iter().filter(|o| matches!(o.origin, Origin::Sugar(_))) {
        set_profile_key(&mut doc, &ov.key, ov.value.clone())?;
    }
    for ov in overrides.iter().filter(|o| matches!(o.origin, Origin::Implied(_))) {
        if profile_key_is_set(&doc, &ov.key) {
            continue;
        }
        set_profile_key(&mut doc, &ov.key, ov.value.clone())?;
    }
    toml::to_string(&doc).map_err(|e| format!("cannot re-serialize the built profile: {e}"))
}

/// The `--show-effective` document: the built profile, with every command-line override rendered as
/// a `#` COMMENT HEADER above it.
///
/// ⚠ Comments, not a table, and deliberately: the whole of stdout stays valid TOML, so
/// `vike-cli backtest run --show-effective … > run.toml` produces a usable profile. Two streams would
/// let a redirect silently drop half the artifact.
///
/// ⚠ **Three origins, not spec §5.3's four.** The ladder is
/// `schema default → file → --set → sugar`, and this side CANNOT SEE a schema default: they live in
/// serde attributes inside `vike-backtest`, which this crate does not link (see the manifest's own
/// comment). So what is rendered is what was OBSERVED on this command line — `--set`, a named sugar
/// flag, and this side's own implied defaults — plus a closing note that every other value came
/// from the profile file or from the engine's default. A fourth column would be a value this
/// command cannot compute.
pub fn render_effective(profile_toml: &str, overrides: &[Override]) -> String {
    // ⚠ The BUILT document, re-parsed, because an `Origin::Implied` row is not necessarily in it —
    // see `implied_row_applied`. A document this side just serialized should always re-parse; if it
    // somehow does not, every row renders unannotated rather than the whole command failing.
    let built: Option<toml::Value> = toml::from_str(profile_toml).ok();
    let mut out = String::new();
    out.push_str("# the resolved profile, and where each command-line value came from\n");
    for ov in overrides {
        let origin = match ov.origin {
            Origin::Set => "--set".to_string(),
            Origin::Sugar(flag) => flag.to_string(),
            // ⚠ THE ROW THAT CAN BE A LIE IF IT IS PRINTED BLIND. `build_profile_toml`'s fourth
            // pass SKIPS an implied row whose key something else already set, so a
            // `--profile tick.toml --show-effective` would otherwise print
            // `data.kind "bar" implied` three lines above a document reading `kind = "tick"`.
            // The row is kept rather than dropped because "this is what would have been implied,
            // and it was not used" is more informative than silence.
            Origin::Implied(reason) => match built.as_ref() {
                Some(doc) if !implied_row_applied(doc, overrides, ov) => {
                    format!("implied ({reason}) — NOT APPLIED, the profile already set this key")
                }
                _ => format!("implied ({reason})"),
            },
        };
        out.push_str(&format!("#   {:<32} {:<24} {origin}\n", ov.key, render_one_value(&ov.value)));
    }
    out.push_str(
        "# every other value is the profile file's, or the engine's own schema default — which \
         this side\n# cannot see (it links no engine crate), so it is not listed.\n\n",
    );
    out.push_str(profile_toml);
    out
}

/// The value `dotted_key` resolves to in `doc`, if any — the one walk both the
/// [`Origin::Implied`] guard and [`render_effective`]'s provenance column read.
fn profile_key_lookup<'a>(doc: &'a toml::Value, dotted_key: &str) -> Option<&'a toml::Value> {
    let mut cur = doc;
    for seg in dotted_key.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

/// Whether `dotted_key` already resolves to something in `doc` — the [`Origin::Implied`] guard.
fn profile_key_is_set(doc: &toml::Value, dotted_key: &str) -> bool {
    profile_key_lookup(doc, dotted_key).is_some()
}

/// Whether an [`Origin::Implied`] row actually reached the BUILT document, for
/// [`render_effective`]'s provenance column.
///
/// ⚠ **Presence in the final document is NOT the discriminator, and reaching for
/// [`profile_key_is_set`] alone here would be a no-op.** By the time the document is built the key
/// is set either way — that is the whole point of the implied pass — so `profile_key_is_set` on the
/// output is unconditionally `true`. What actually separates the two cases is WHO set it, and there
/// are exactly two ways an implied row loses:
///
/// * another override on this same command line names the same key (`--kind tick`, or
///   `--set data.kind=tick`) — those run in earlier passes and win outright; or
/// * the `--profile` file set it, in which case the built document holds the FILE's value rather
///   than this row's.
///
/// The second test is a value comparison, not a presence one. It has one indistinguishable case,
/// and it is harmless: a file that set the key to the very value this row would have implied reads
/// as "applied". The rendered value is right either way, so nobody is misled about the run.
fn implied_row_applied(doc: &toml::Value, overrides: &[Override], row: &Override) -> bool {
    if overrides.iter().any(|o| o.key == row.key && !matches!(o.origin, Origin::Implied(_))) {
        return false;
    }
    profile_key_lookup(doc, &row.key) == Some(&row.value)
}

/// One override's value as a SINGLE comment-line fragment.
///
/// ⚠ **The one-line property is load-bearing**, not cosmetic. [`render_effective`] puts this inside
/// a `#` comment so that the whole of `--show-effective`'s stdout stays valid TOML and
/// `… --show-effective > run.toml` writes a usable profile. A TABLE value — reachable today through
/// `--set engine.impact={model="sqrt"}`, which nothing refuses ([`set_profile_key`]'s table check
/// fires only when the LEAF is already a table) — serializes as a `[v]` SECTION with its keys on
/// FOLLOWING lines, and a multi-line string value can do the same. Either one would put a raw
/// newline inside the comment and the redirect would write a file that does not parse.
///
/// So only a value that round-trips as a single `v = …` line is rendered; anything else says so and
/// points at the document below, which carries it correctly.
fn render_one_value(value: &toml::Value) -> String {
    let doc = toml::Value::Table([("v".to_string(), value.clone())].into_iter().collect());
    match toml::to_string(&doc) {
        Ok(s) => match s.trim().strip_prefix("v = ") {
            Some(scalar) if !scalar.contains('\n') => scalar.to_string(),
            _ => "<multi-line — see the document below>".to_string(),
        },
        Err(_) => String::from("<unrenderable>"),
    }
}

#[path = "backtest_tests.rs"]
#[cfg(test)]
mod backtest_tests;
