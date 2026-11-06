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
//! action has one spelling. A missing sub-verb prints the plane's help (`LONG_ABOUT`) and exits `2`, the way
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
//! - `--addr <host:port>` (default `127.0.0.1:7880`): the backtesthub's address — `vike-backend
//!   backtesthub --addr`, NOT the datahub, since ruling 7 split the served surface. It binds
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
//! That predicate is `BacktestProfile::is_paramscan`, which the engine and the backtesthub BOTH
//! branch on, and [`declares_a_paramscan_grid`] is this side's presence check over the same key — so
//! all three answer the same question the same way. ⚠ It closes a divergence that predates the
//! merge and is worth stating: the remote arm used to call `run_backtest` unconditionally, whose
//! server-side runner ignored the grid and reported ONE point (it refuses a grid profile now),
//! while `--local` handed the same profile to an engine that ran the whole grid. Same command
//! line, two different computations, nothing in the output saying which.
//!
//! ⚠ **`--local` and `--addr` run the same search, and stage 7 is what made that true.**
//! `--optimizer grid|euler|tpe|genetic`, `--euler-depth`, `--trials`, `--seed` and
//! `--rank-by multi` all work on both routes: `vike_datahub_client::proto`'s
//! `Request::RunParamscanProfile` carries a `search` selector, and both sides resolve it through the
//! one `vike_backtest::search::select`, so a knob refused under the wrong method is refused
//! with the same sentence whichever route ran. The three method KNOBS are forwarded as the TOKENS
//! you typed — this side parses none of them, which is what keeps that sentence single.
//!
//! ⚠ **The two SELECTORS are the exception, and they are canonicalised rather than parsed.** They
//! name members of a declared roster, so `vike_model::run_request::flag_vocab`'s `accept_value`
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
//! walk-forward is an OPERATION of the backtest plane, and the top level names planes. What the
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
//! `--local` is for the box that already HAS the tape: it drives the standalone `backtesthub` engine
//! — **spawned, never linked**, because this crate's identity is being DataFusion-free
//! ([`crate::cmd::engine`] carries the whole argument and the search order). `--engine` names the
//! engine outright and is refused without `--local`, where it would describe the wrong machine.
//!
//! ⚠ **`--local` moves the ENGINE, not the HISTORY.** Since 2026-09-25 every history read goes
//! through a datahub (decision 0084), so the spawned engine dials its `config.datahub_addr` row (loopback by
//! default) like every other reader. For tape on this box, start a key-less datahub on it first:
//! `vike-backend datahub --store DIR`. `--store DIR` named that directory until then and
//! is now refused BY NAME on both arms, with that command in the refusal
//! (`vike_model::run_request::flag_vocab::store_flag_removed`).
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

mod execute;
pub(super) mod profile;
pub(crate) mod read;
mod render;
mod route;
mod run_args;

use std::path::Path;
use std::process::ExitCode;

#[cfg(test)]
use serde_json::Value;
#[cfg(doc)]
use vike_datahub_client::DatahubClient;
use vike_datahub_client::WireSearch;
use vike_hub_proto::auth::HubKeys;
#[cfg(test)]
use vike_model::run_request::flag_vocab;

use crate::cmd::args;

use execute::execute;
#[cfg(doc)]
use execute::execute_local;
#[cfg(test)]
use profile::{build_profile_toml, inject_script_src, merge_preset_params, parse_scalar};
#[cfg(doc)]
use profile::{build_profile_toml, merge_preset_params};
#[cfg(test)]
use profile::{render_effective, set_profile_key};
#[cfg(test)]
use read::parse_read;
#[cfg(test)]
use read::{READ_SUBCOMMANDS, ReadArgs};
use read::{ReadSub, execute_read};
#[cfg(test)]
use render::{cell, ledgered_head};
#[cfg(test)]
use route::{Route, declares_a_paramscan_grid, refuse_a_walkforward_flag, route_of};
#[cfg(doc)]
use route::{WALKFORWARD_HAS_NO_LOCAL_ARM, declares_a_paramscan_grid, route_of};
#[cfg(test)]
use run_args::PARAMS_REFUSED;
#[cfg(test)]
use run_args::parse_run_args;
pub use run_args::{PROFILE_TOP_LEVEL_KEYS, SUGAR, Shape, Sugar};

/// The backtesthub's address, folded from the three rungs ruling 7 names: `--addr <v>` →
/// the `config.backtest_addr` setting → [`vike_config::DEFAULT_BACKTEST_ADDR`].
///
/// ⚠ **This function exists because a literal used to sit where the last rung is.** The verb
/// carried its own `const DEFAULT_ADDR`, which had two costs and the second is the one that
/// matters: an operator's `backtest_addr` was read by nothing here, so a box configured for its
/// backtesthub still dialled the compiled-in address — and a compiled-in address is exactly
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
/// `walkforward`), then "TWO of the THREE backtest verbs … and the third is a KNOWN GAP" (which
/// missed `mcp`, whose compute tools have the same hole). A reader greps for who ignores the
/// setting and believes whatever the count rules out.
///
/// `git grep -l DEFAULT_BACKTEST_ADDR -- crates/vike-cli/src` is the roster — every dialer names
/// that constant, and the dispatcher that decides who gets the SETTING is in the answer too. It
/// carries no number, so it cannot rot when another dialer appears, and
/// `crates/vike-ops/tests/docs/unrun_command_gate.rs` RUNS it, so it cannot quietly stop answering
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

// ⚠ **`SRC_KEY` USED TO BE A CONST HERE** — the `[strategy.params]` key carrying a Rhai strategy's
// SOURCE (what `--script` sets, and the one key a `--preset` file may not define), and
// `docs/decisions/0064-a-named-run-carries-no-source.md` cites it. It had three homes — here,
// `crates/vike-studio-core/src/user_strategies/load.rs` and the bare literal in
// `crates/vike-backtest/src/harness/registry.rs`'s `rhai_overrides` — until that decision's fourth
// reader moved the fact down to `vike_model::RESERVED_SRC_KEY`, after which this was a same-word
// alias. A symbol has one name: `crates/vike-cli/src/cmd/backtest/profile.rs` names the vike-model
// constant directly.

/// The container key a preset must NOT wrap its knobs in — see [`merge_preset_params`].
const PARAMS_WRAPPER_KEY: &str = "params";

// ⚠ **`RANK_METRICS` and `OPTIMIZERS` USED TO BE TWO CONSTS HERE, AND THE ROSTERS THEY HELD NOW
// LIVE IN THE PROTOCOL CRATE.** `--rank-by` and `--optimizer` are spelling-checked against
// `vike_model::run_request::flag_vocab::BACKTEST_FLAGS` — through `flag_vocab::accept_value`, on this
// file's two selector arms in `parse_run_args` — so neither the roster nor the MATCH RULE is
// written down twice any more. This block is the tombstone rather than a `pub use`, which this
// workspace refuses on a move; what it keeps is the two arguments that outlived the consts.
//
// **Why the check happens here at all**, which is unchanged: it is a SPELLING check and never a
// second implementation — what a metric or a method MEANS is `vike_backtest::harness`, on
// whichever side runs — and catching a typo before the dial or the spawn is what makes it a local
// usage error instead of a wasted round trip. `crate::cmd::data`'s `on_gap_override` states the same
// rule for its own two rosters, in the same words (and reads `vike_model::run_request::gap`'s arrays).
//
// **What the two consts cost, kept because each is the reason the shared vocabulary exists:**
//   * `OPTIMIZERS` was a THREE-element array refusing `genetic` during arg parsing while `grid`
//     was the only name with a remote route. Stage 7 of
//     `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` collapsed three rosters
//     into one — the wire carries a method (`Request::RunParamscanProfile`'s `search`) — and the
//     roster moved to the protocol crate (and from there to `vike-model`'s `run_request`, when the
//     flag vocabulary needed it) because that is the only crate this one and
//     `vike-backtest` both depend on. It had been `= vike_model::run_request::search_methods::SEARCH_METHODS` ever
//     since, so its MEMBERS could not drift; its MATCH RULE could, and did.
//   * `RANK_METRICS` was a five-element literal to the end, and the drift it hid was the rule
//     rather than the names: `one_of` compared with `contains` — exact, case-sensitive — while
//     `crates/vike-backtest/src/harness/sweep/rank.rs`'s `RankMetric::from_str_ci` lowercases and
//     `crates/vike-backtest/src/search/select.rs`'s `resolve` uses
//     `eq_ignore_ascii_case`. So `--rank-by SHARPE` was a local exit-2 here and a legal engine
//     value one crate over, and `--optimizer TPE` with it. `flag_vocab`'s own doc carries the
//     measurement and argues the direction of the fix: widening the client is the direction in
//     which nothing that works stops working.
//
// ⚠ `multi` reaches BOTH routes since stage 7, and `RANK_METRICS`'s doc said the opposite for a
// while. It used to be LOCAL-ONLY as a WIRE fact: the server resolved `rank_by` through
// `harness::RankMetric::from_str_ci`, whose four arms have no `multi`. It resolves through
// `search::select`'s `resolve_rank` now, which has FIVE, and a daemon too old to do that is refused
// BY NAME through `vike_datahub_client::FEATURE_SEARCH_METHOD` with nothing sent.

/// The default `--optimizer`, spelled through the protocol crate for the same reason the roster is.
///
/// ⚠ **TEST-ONLY, and that is a fact about this side rather than an omission.** This crate
/// substitutes no default: an absent `--optimizer` is forwarded as `None` on BOTH routes, and the
/// side that RUNS resolves it (`vike_backtest::search::select::resolve` against
/// `DEFAULT_SEARCH_METHOD`). A lib-side default here would be a second answer to "what runs when
/// nobody said", which is the class this stage exists to remove. The tests below need the name to
/// build a valid sample argv, so it is gated rather than invented or deleted.
#[cfg(test)]
const DEFAULT_OPTIMIZER: &str = vike_model::run_request::search_methods::DEFAULT_SEARCH_METHOD;

/// The program name this plane is read under.
const BIN: &str = "vike-cli backtest";

// The prose half of `backtest --help`; the usage line, the sub-verb roster and each sub-verb's option
// table are `clap`'s, derived from [`Cli`], [`run_args::RunCmd`] and [`read::ReadCmd`].
const LONG_ABOUT: &str = "\
backtest is a PLANE: a sub-verb is always required, so one action has one spelling.

  run          compute a backtest. The resolved PROFILE says what, on TWO independent axes: a
               non-empty [paramscan] grid searches the parameter space, a [walkforward] table walks
               the run forward over out-of-sample windows, and a profile declaring both is sent
               WHOLE — that table's own keys say what each window searches
  ls           list what past runs left behind — filtered, ordered and shaped
  show         re-render ONE stored run, or export one of its stored documents raw
  path         print one absolute path inside a run directory, so `$(…)` works
  tag          label a run, note it, or give it a MARK a later comparison can name
  diff         what differed in the INPUTS beside what differed in the OUTPUTS, for two runs
  gate         judge a run against a mark and turn that verdict into an EXIT CODE
  params       print a Rhai script's (or a named strategy's) tunable param(name, default) knobs.
               OFFLINE — no server, no store, no engine
  strategies   the strategy roster the backtesthub can run
  templates    the starter Rhai strategies this binary ships — list them, print one, or save one
               to a NEW file. OFFLINE
  script-api   everything an authored Rhai strategy may CALL: the reads, the order verbs, the
               knob, the indicator spellings — and the hooks to put them in. OFFLINE
  script-check compile one Rhai script and answer with the diagnostic and an EXIT CODE, for a save
               hook or a pre-commit. OFFLINE";

/// The `backtest` plane's command line.
///
/// ⚠ **The sub-verb is OPTIONAL to `clap` and REQUIRED by [`parse`]**, and the split is so that the
/// retired `--list-params` spelling (a MODE FLAG on this verb for months, answered by name rather
/// than as an unknown argument — the same reason `crate::RETIRED_COMMANDS` exists one level up)
/// reaches its replacement message instead of `clap`'s "a subcommand is required". There is no
/// bare `vike-cli backtest …` — decision 11 of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`.
#[derive(clap::Parser, Debug)]
#[command(
    about = "Compute a strategy over history and judge what came back",
    long_about = LONG_ABOUT
)]
struct Cli {
    #[command(subcommand)]
    sub: Option<Sub>,
    #[arg(long, hide = true)]
    list_params: bool,
}

/// Which sub-verb ran, with its arguments. Adding one is a variant here and an arm in [`run`]'s
/// reading half, and `every_subcommand_is_reachable_by_the_name_it_advertises` holds the roster
/// honest.
///
/// ⚠ **The reading half is NESTED in spirit and FLAT in type**: nine variants carry the one
/// [`read::ReadCmd`] flag set, and [`Sub::into_read`] names the [`ReadSub`] each stands for — the
/// type the reading plane's arity rules, foreign-flag refusals and tests have always named.
#[derive(clap::Subcommand, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "one parsed command line per process; boxing buys nothing"
)]
enum Sub {
    /// Compute a backtest: ship a profile to the backtesthub, or run the engine here
    Run(run_args::RunCmd),
    /// List what past runs left behind — filtered, ordered and shaped
    Ls(read::ReadCmd),
    /// Re-render ONE stored run, or export one of its stored documents raw
    Show(read::ReadCmd),
    /// Print one absolute path inside a run directory, so `$(…)` works
    Path(read::ReadCmd),
    /// Label a run, note it, or give it a MARK a later comparison can name
    Tag(read::ReadCmd),
    /// What differed in the INPUTS beside what differed in the OUTPUTS, for two runs
    Diff(read::ReadCmd),
    /// Judge a run against a mark and turn that verdict into an EXIT CODE
    Gate(read::ReadCmd),
    /// Print a Rhai script's (or a named strategy's) tunable param(name, default) knobs. OFFLINE
    Params(read::ReadCmd),
    /// The strategy roster the backtesthub can run
    Strategies(read::ReadCmd),
    /// The starter Rhai strategies this binary ships — list them, print one, or save one. OFFLINE
    Templates(read::ReadCmd),
    /// Everything an authored Rhai strategy may CALL, and the hooks to put them in. OFFLINE
    ScriptApi(read::ReadCmd),
    /// Compile one Rhai script and answer with the diagnostic and an EXIT CODE. OFFLINE
    ScriptCheck(read::ReadCmd),
}

impl Sub {
    /// The reading sub-verb and its arguments, or `None` for `run`.
    fn into_read(self) -> Option<(ReadSub, read::ReadCmd)> {
        Some(match self {
            Sub::Run(_) => return None,
            Sub::Ls(c) => (ReadSub::Ls, c),
            Sub::Show(c) => (ReadSub::Show, c),
            Sub::Path(c) => (ReadSub::Path, c),
            Sub::Tag(c) => (ReadSub::Tag, c),
            Sub::Diff(c) => (ReadSub::Diff, c),
            Sub::Gate(c) => (ReadSub::Gate, c),
            Sub::Params(c) => (ReadSub::Params, c),
            Sub::Strategies(c) => (ReadSub::Strategies, c),
            Sub::Templates(c) => (ReadSub::Templates, c),
            Sub::ScriptApi(c) => (ReadSub::ScriptApi, c),
            Sub::ScriptCheck(c) => (ReadSub::ScriptCheck, c),
        })
    }
}

/// The `backtest` plane as a `clap` command — what `--help`, the parser and the published
/// reference's gate all describe.
pub(crate) fn command() -> clap::Command {
    args::command::<Cli>(BIN)
}

/// Read a `backtest` argv tail — everything after the verb word, sub-verb included.
///
/// A missing sub-verb is refused with the roster DERIVED from the command, so it can never name
/// the set short (the hand-written copy `crate::cmd::data`'s replaced omitted a verb that had
/// shipped, on the verb that deletes).
///
/// ⚠ **History, kept so a citation by symbol still lands.** This plane used to carry its own
/// roster (`SUBCOMMANDS`, the hand-kept `USAGE` page, with `Sub::as_str` and a `subcommand_roster()` that rendered it into
/// every refusal) and its own router (`claim_subcommand`); the published reference still cites
/// them in the past tense. `clap` owns both now, and `crate::surface::SUB_VERBS` is held to the
/// command by `the_surface_names_exactly_the_sub_verbs_this_plane_dispatches`.
fn parse(argv: impl IntoIterator<Item = String>) -> Result<Sub, args::Stop> {
    let argv: Vec<String> = argv.into_iter().collect();
    let cli: Cli = match args::parse(BIN, argv.clone()) {
        Ok(cli) => cli,
        // ⚠ The retired spelling answers with its replacement wherever it sits on the line:
        // `backtest --list-params --script s.rhai` fails to parse as `clap` reads it (the plane has
        // no `--script`), and "unexpected argument" would hide the one sentence that says where
        // the flag went.
        Err(stop) if !stop.is_help() && argv.iter().any(|a| a == "--list-params") => {
            return Err(args::Stop::message(BIN, LIST_PARAMS_RETIRED));
        }
        Err(stop) => return Err(stop),
    };
    match cli.sub {
        Some(sub) => Ok(sub),
        None if cli.list_params => Err(args::Stop::message(BIN, LIST_PARAMS_RETIRED)),
        None => {
            let roster = command()
                .get_subcommands()
                .filter(|s| !s.is_hide_set())
                .map(|s| s.get_name().to_string())
                .collect::<Vec<_>>()
                .join(" | ");
            Err(args::Stop::message(BIN, format!("a subcommand is required ({roster})")))
        }
    }
}

/// `--list-params` was a MODE FLAG on this verb for months. Decision 11 gives it a sub-verb, and
/// the old spelling answers with the new one rather than with "unknown argument" — the same reason
/// `crate::RETIRED_COMMANDS` exists one level up.
const LIST_PARAMS_RETIRED: &str = "--list-params is now a subcommand: `vike-cli backtest params --script <s.rhai>`. A discovery \
     runs no backtest at all, and decision 11 of the backtest-CLI-surface design gives every \
     action inside this plane exactly one spelling.";

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
    /// `--addr <host:port>`: the backtesthub this run dials, UNRESOLVED. The parser cannot see
    /// `config.backtest_addr` — only the composition root does — so the ladder is folded by
    /// [`resolve_addr`] inside `execute`, and never here.
    addr: Option<String>,
    json: bool,
    /// `--local`: run on THIS machine by driving the standalone engine, instead of shipping the
    /// profile to the backtesthub. It moves the ENGINE, not the history: the child still reads
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
    /// (`crates/vike-backtesthub/src/backtest_cli/search_flags.rs`'s `parse_search_flags`) and this
    /// side does not second-guess it.
    ///
    /// ⚠ CANONICAL, for [`Args::optimizer`]'s reason — `--rank-by SHARPE` is held here as
    /// `sharpe`, so the token that reaches the wire, the spawned engine and the run ARTIFACT's
    /// identity is the one spelling. (`vike_datahub_client::client`'s `run_paramscan_profile`
    /// needed no help for its own `multi` capability pre-check: that one already compares with
    /// `eq_ignore_ascii_case`. What canonicalising buys is every reader FURTHER down agreeing
    /// about what was asked for.)
    rank_by: Option<String>,
    /// `--optimizer`: the search METHOD, spelling-checked against
    /// `vike_model::run_request::flag_vocab`'s `--optimizer` row and otherwise forwarded. `None` is
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
    /// ⚠ Nothing is parsed here and nothing may be. `vike_backtest::search::select` is the
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

/// Entry point the dispatcher routes to. `argv` is everything AFTER the `backtest` verb;
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
/// finished writing" row in every listing. `vike_paths::state_path::MARKS_SUBDIR` carries the
/// argument.
///
/// ⚠ The sub-verb is part of the argv `clap` reads ([`parse`]): `run` takes its own profile-building
/// flags ([`run_args::RunCmd`]) and the nine reading verbs share one flag set
/// ([`read::ReadCmd`]).
pub fn run(
    argv: impl Iterator<Item = String>,
    project_root: Option<&Path>,
    user_data_dir: Option<&Path>,
    now: i64,
    configured_addr: Option<&str>,
    keys: Option<&HubKeys>,
) -> ExitCode {
    let runs_root = user_data_dir.map(|d| d.join(vike_paths::state_path::RUNS_SUBDIR));
    let marks_root = user_data_dir.map(|d| d.join(vike_paths::state_path::MARKS_SUBDIR));
    let sub = match parse(argv) {
        Ok(s) => s,
        Err(stop) => return args::finish(&stop),
    };
    match sub {
        Sub::Run(cmd) => {
            let args = match cmd.into_args() {
                Ok(a) => a,
                Err(msg) => return args::refuse("vike-cli backtest run", msg),
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
        reading => {
            let (word, cmd) =
                reading.into_read().expect("every sub-verb but `run` is a reading one");
            let parsed = match cmd.into_args(word) {
                Ok(a) => a,
                Err(msg) => return args::refuse(BIN, msg),
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
/// `backtest`? Routed exactly as [`run`] routes it, so a line this answers `Ok` for is a line the
/// binary would get as far as EXECUTING. For tests in sibling modules that print a `vike-cli
/// backtest …` line; `crate::cmd::accepts` is the entry they call.
#[cfg(test)]
pub(super) fn accepts(argv: &[&str]) -> Result<(), String> {
    match parse(argv.iter().map(|s| (*s).to_string())).map_err(|stop| stop.to_string())? {
        Sub::Run(cmd) => cmd.into_args().map(|_| ()),
        reading => {
            let (word, cmd) =
                reading.into_read().expect("every sub-verb but `run` is a reading one");
            cmd.into_args(word).map(|_| ())
        }
    }
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

#[path = "backtest/tests.rs"]
#[cfg(test)]
mod backtest_tests;
