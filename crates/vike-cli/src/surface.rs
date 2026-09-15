//! **The `backtest` plane's command surface, as DATA** — the one table every consumer renders
//! from, instead of restating it.
//!
//! # Why this exists
//!
//! `vike-cli backtest` is not one parser but three levels — [`crate::run`], then
//! `crate::cmd::backtest`'s `claim_subcommand`, then one of two flag loops — and almost every fact
//! a reader needs is encoded as CONTROL FLOW rather than data: whether a flag takes a value is the
//! difference between a `flags.value` call and an `args::no_value` call inside a match arm.
//!
//! Because the surface is not data, every consumer restates it, and the restatements rot.
//! MEASURED, not feared: `crate::cmd::backtest`'s `USAGE` advertises none of `--rank-by`,
//! `--optimizer`, `--euler-depth`, `--trials` or `--seed`, all five of which its `parse_run_args`
//! accepts — and CI is green, because every gate over that const is a substring check over sub-verb
//! NAMES and none of them can see a missing flag.
//!
//! # What it is for
//!
//! [`rendered_files`] returns the published assets by name. The docs site fetches them from the
//! public mirror's latest release and renders committed MDX from them at prebuild, so **the
//! Rust-side obligation is an ASSET, not a renderer** — this module produces bytes and makes no
//! decision about a page.
//!
//! A `pub fn` rather than a bin-only path, deliberately: the gate below checks the bytes
//! **in-process** — no subprocess, no `CARGO_BIN_EXE`, no scratch directory and **no skip path**.
//! That is the strongest argument for this shape, and the precedent is `crate::cmd::mcp`'s
//! `the_registry_manifest_lists_every_tool_this_server_serves`, which compares a committed,
//! externally-published manifest to the live registry the same way.
//!
//! ⚠ **This module is DATA and carries no argument.** Rationale belongs in the module docs of the
//! code it describes and in `crates/vike-cli/CLAUDE.md`; a row here carries what a renderer needs
//! plus the `evidence` naming where it was read from.
//!
//! ⚠ **Key SETS are the contract, never key ORDER.** `serde_json`'s map flavour flips with
//! `preserve_order`, which the workspace's DataFusion crates enable through feature unification
//! whenever they share a build. `vike-cli` is DataFusion-free by construction, so a standalone run
//! is stable while a shared build may not be — the divergence is exactly gate-versus-release.
//! Everything order-bearing here is an ARRAY carrying an explicit name field.

use std::collections::BTreeMap;

/// The file name this module publishes, and the name the far side's asset list must carry.
///
/// ⚠ It reaches that list as a BARE LITERAL, because `vike-ops` — which owns the gate holding the
/// rendered asset set equal to the release workflow's and the mirror's — cannot depend on this
/// crate: `crates/vike-cli/Cargo.toml` declares the top layer and `crates/vike-ops/tests/layer_gate.rs`
/// fails the edge. That is the same declared hole `bins.json` already carries, and it is recorded
/// here rather than closed.
pub const CLI_JSON: &str = "cli.json";

/// The schema version, bumped on any change a consumer could not read.
///
/// ⚠ **Nothing enforces this number yet, and the sentence that used to stand here said otherwise.**
/// It claimed an asset "reporting a schema the site does not read" demotes every asset to the
/// committed snapshot. MEASURED against the far side's `docs-site/scripts/fetch-docs-data.mjs`:
/// that check reads ONE asset's version — it parses `texts[ASSETS.indexOf('stats.json')]` and
/// compares only `stats.schema_version`. A `cli.json` declaring a version the site cannot read
/// sails through and is rendered anyway. (The far side's own comment carries the same overclaim,
/// which is where this one was copied from.)
///
/// What IS true, and is the half worth carrying: the fetch is ALL-OR-NOTHING. Every asset is
/// fetched in one `Promise.all`, and any one of them failing to fetch or failing to parse demotes
/// EVERY asset to the committed snapshot, silently and without erroring. So a malformed `cli.json`
/// does take the whole site back a release — just for being malformed, not for its version.
///
/// So this constant is a DECLARATION, not a gate: bumping it protects nobody until the far side
/// reads it. A bump is still a cross-repo change, and the far side's read is what has to land with
/// it. Until then, treat a shape change as breaking regardless of what this number says.
pub const SCHEMA_VERSION: u32 = 1;

/// The plane this table describes.
pub const PLANE: &str = "backtest";

/// The sub-verb roster, in the order `crate::cmd::backtest`'s `SUBCOMMANDS` declares it.
///
/// ⚠ **The sub-verb is always required** — there is no bare `vike-cli backtest …` that means `run`.
pub const SUB_VERBS: &[&str] =
    &["run", "ls", "show", "path", "tag", "diff", "gate", "params", "strategies"];

/// Whether a flag takes a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    /// A bare switch. `args::no_value` refuses the `--flag=v` spelling on one.
    None,
    /// Takes a value, in either the `--flag v` or the `--flag=v` spelling.
    Required,
}

/// What repeating a flag does.
///
/// ⚠ **Four values, and the commonest is the one nobody names.** Most `Option`-assigning arms
/// silently LAST-WIN: `--profile a.toml --profile b.toml` runs `b.toml` with no refusal and no
/// warning. A three-valued field would have nothing to put in two-thirds of these rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    /// Silently overwrites. `--venue a --venue b` resolves to `b`, and the first value is gone.
    ///
    /// ⚠ This is an OPERATOR-VISIBLE fact, not a parser-internal one. The parser accumulates into
    /// an override vector; `build_profile_toml` then replays that vector's sugar pass onto the SAME
    /// key, so accumulation inside is last-wins outside.
    LastWins,
    /// Every occurrence is kept.
    Accumulate,
    /// Repeatable AND comma-splitting, folded into ONE override after the loop, in argv order.
    CommaListFold,
    /// A bare switch: repeating discards no earlier value and changes no outcome.
    ///
    /// Distinct from [`Repeat::LastWins`], which asserts an earlier value WAS overwritten — a
    /// boolean has none, so that category would be a false statement about it.
    Idempotent,
}

/// Whether a spelling reaches behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Accepted and threaded.
    Ships,
    /// Parsed and refused BY NAME, with a reason naming what is missing. Those reasons are the most
    /// useful rows a page can carry: they answer "why did my command fail".
    Unbuilt,
    /// Answered with its replacement rather than with "unknown".
    Retired,
}

/// One refusal.
///
/// ⚠ **Keyed on the SUB-VERB**, because a refusal message is a function of (flag, sub-verb) rather
/// than a per-flag constant. `--cash` gets a sentence naming `run` on `params`, a bare unknown-option
/// on the other six reading verbs, and is accepted on `run` — one flag, three answers.
#[derive(Debug, Clone, Copy)]
pub struct Refusal {
    /// The sub-verb (or the condition) this message answers.
    pub on: &'static str,
    /// The message VERBATIM. A `format!` hole is rendered as its template.
    pub message: &'static str,
}

/// One flag.
#[derive(Debug, Clone, Copy)]
pub struct FlagRow {
    pub long: &'static str,
    pub value: Value,
    /// The metavar, taken from the crate's own advertised spelling where two exist.
    pub value_name: Option<&'static str>,
    /// A value that PARSES today. This is what retires the two hand-written arity tables.
    pub sample: Option<&'static str>,
    pub repeat: Repeat,
    /// The sub-verbs that ACCEPT it.
    ///
    /// ⚠ EMPTY for a [`Status::Unbuilt`] flag: it accepts nowhere. Where its refusal reaches is a
    /// different fact and lives in [`Self::refusal_reaches`] — a consumer reading this field as
    /// "accepts" would otherwise publish four flags that accept nothing as accepted everywhere.
    pub applies_to: &'static [&'static str],
    /// Where the refusal reaches, for a flag that is accepted nowhere.
    pub refusal_reaches: &'static [&'static str],
    pub status: Status,
    /// The roster its value is checked against, if any.
    pub roster_id: Option<&'static str>,
    /// `cli` | `engine` | `none` — which side spelling-checks the value. Several knobs are forwarded
    /// VERBATIM because the engine owns their ranges and caps.
    pub validated_by: &'static str,
    /// The dotted profile key it writes, if it is sugar for one.
    pub profile_key: Option<&'static str>,
    pub shape: Option<&'static str>,
    /// `any` | `local` | `remote`.
    pub mode_scope: &'static str,
    /// `schema` | `implied` | `far_side` | `none`.
    ///
    /// ⚠ `far_side` exists because this crate deliberately REFUSES to assert some defaults — a
    /// table printing one would publish a fact the code specifically declines to state.
    pub default_kind: &'static str,
    pub default_value: Option<&'static str>,
    /// Set when the default applies only under another flag, so a flat rendering cannot claim it
    /// unconditionally.
    pub default_conditional_on: Option<&'static str>,
    pub default_reason: Option<&'static str>,
    /// One line, safe inside a markdown table cell.
    pub short: &'static str,
    /// `path`'s `symbol` — never a line number.
    pub evidence: &'static str,
    pub refusals: &'static [Refusal],
}

/// One value roster.
#[derive(Debug, Clone, Copy)]
pub struct RosterRow {
    pub id: &'static str,
    pub members: &'static [&'static str],
    /// Present in the roster and REFUSED, with its own sentence. A plain string list would render
    /// such a member as available, or omit it — and both are wrong.
    pub refused_members: &'static [&'static str],
    /// `exact` | `ascii_ci` | `open_tail`.
    ///
    /// ⚠ Required, and the field nobody would think to add: two rosters in this tree hold the same
    /// spellings and disagree about what MATCHES them, because one compares with `contains` and the
    /// other lowercases first. A table printing the names states nothing about which spellings work.
    ///
    /// ⚠ `open_tail` is NOT a closed set. Rendering one as an exhaustive list publishes a list that
    /// is wrong by omission.
    pub match_rule: &'static str,
    /// The Rust path this is derived from, or `None` — which REQUIRES an [`Self::admission`].
    pub derived_from: Option<&'static str>,
    /// Why nothing derives it. The shape `vike_config::CONSUMPTION` uses for settings keys, so an
    /// ungated hand copy is visible rather than assumed away.
    pub admission: Option<&'static str>,
    pub evidence: &'static str,
}

/// Every flag on the plane, one row each.
pub const FLAGS: [FlagRow; 49] = [
    FlagRow {
        long: "--add",
        value: Value::Required,
        value_name: Some("TAG"),
        sample: Some("ci"),
        repeat: Repeat::Accumulate,
        applies_to: &["tag"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: None,
        shape: Some("string"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some("Absent = no labels written."),
        short: "`tag` only, and THE ONLY REPEATABLE FLAG IN THE READING FLAG LOOP — `--add ci --add fee-fix` is two labels, pushed onto `ReadArgs::add`. Writes into the run's own META_FILE (meta.json) sidecar via `vike_model::runs::add_tags`.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `parse_read` (the `\"--add\"` arm calls `a.add.push`, with the ⚠ comment naming it the only repeatable one); crates/vike-cli/src/cmd/runs/tag.rs's `run_tag`",
        refusals: &[Refusal {
            on: "ls, show, path, diff, gate, params, strategies",
            message: "--add does not apply to `backtest {sub}` — that flag WRITES a label, a note or a mark onto a stored run, which is what `tag` does",
        }],
    },
    FlagRow {
        long: "--addr",
        value: Value::Required,
        value_name: Some("host:port"),
        sample: Some("127.0.0.1:7880"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: None,
        shape: Some(
            "host:port, UNRESOLVED by the parser — the ladder is folded in `execute` by `resolve_addr`",
        ),
        mode_scope: "remote",
        default_kind: "implied",
        default_value: Some("127.0.0.1:7880"),
        default_conditional_on: None,
        default_reason: Some(
            "`resolve_addr(cli, configured)` folds three rungs and falls through to `vike_config::DEFAULT_BACKTEST_ADDR`. The FLAG layer performs no host:port check at all (a blank value is treated as absent, and dialling anything else produces a connect-rung failure); the file and env layers do check, via `vike_config`'s `check_addr` / `check_env_addr`.",
        ),
        short: "The COMPUTE daemon to dial (vike-backend backtest --addr, NOT the datahub). Refused beside --local; unwritten, it falls through config.backtest_addr to 127.0.0.1:7880.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `resolve_addr`, `Args::addr` and `execute`; crates/vike-config/src/config.rs's `DEFAULT_BACKTEST_ADDR`, `Config::backtest_addr`, `BACKTEST_ADDR_ENV` and `check_env_addr`; crates/vike-cli/src/lib.rs's `Resolved::backtest_addr`",
        refusals: &[
            Refusal {
                on: "--addr combined with --local (CLI, exit 2)",
                message: "--addr names a remote backtest daemon, so it cannot be combined with --local\ndrop one: --local runs the engine on this machine, --addr ships the profile to a server",
            },
            Refusal {
                on: "the daemon is not reachable (exit 3, the CONNECT rung; `{addr}` is the resolved address and `{e}` the transport error)",
                message: "cannot connect to the backtest daemon at {addr}: {e} (start it with `vike-backend backtest --addr`)",
            },
            Refusal { on: "written with no value", message: "--addr requires a value" },
        ],
    },
    FlagRow {
        long: "--against",
        value: Value::Required,
        value_name: Some("<mark>"),
        sample: Some("@baseline/momentum"),
        repeat: Repeat::LastWins,
        applies_to: &["gate"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: Some("selector_forms"),
        validated_by: "cli",
        profile_key: None,
        shape: Some("run selector"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "No default — REQUIRED on `gate`, refused at parse time when absent or blank.",
        ),
        short: "`gate` only, and REQUIRED. A blank/whitespace value counts as absent. It is the baseline every criterion is relative to; a baseline that will not resolve is `Exit::Failed`/`Exit::Empty`, never `Exit::Breach`.",
        evidence: "crates/vike-cli/src/cmd/runs/gate.rs's `refuse_an_ungateable_line` and `run_gate`",
        refusals: &[
            Refusal {
                on: "gate, absent or blank",
                message: "`backtest gate` needs `--against <run|mark>` — every criterion is RELATIVE to a baseline, so there is nothing to judge without one. Make a stable baseline with `vike-cli backtest tag <run> --as baseline/<name>`, then gate `--against @baseline/<name>`. (For an ABSOLUTE threshold this grammar cannot spell, pipe `vike-cli backtest show <run> --json` through `jq`.)",
            },
            Refusal {
                on: "ls, show, path, tag, diff, params, strategies",
                message: "--against does not apply to `backtest {sub}` — that flag names the BASELINE or the criteria a verdict is computed from, and `gate` is the verb whose product is that verdict",
            },
        ],
    },
    FlagRow {
        long: "--all",
        value: Value::None,
        value_name: None,
        sample: None,
        repeat: Repeat::Idempotent,
        applies_to: &["diff"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: None,
        shape: Some("bool"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some("Absent = changed rows only."),
        short: "`diff` only. Shows UNCHANGED leaves too — the negation of the default. Read by `run_diff` as `show_all` and passed to all three renderers (human/md/json).",
        evidence: "crates/vike-cli/src/cmd/runs/diff.rs's `run_diff` (`let show_all = a.all;`)",
        refusals: &[
            Refusal {
                on: "ls, show, path, tag, gate, params, strategies",
                message: "--all does not apply to `backtest {sub}` — that flag shapes a two-run COMPARISON, which is what `diff` renders",
            },
            Refusal { on: "(any reading sub-verb, with a value)", message: "--all takes no value" },
        ],
    },
    FlagRow {
        long: "--as",
        value: Value::Required,
        value_name: Some("NAME"),
        sample: Some("baseline/momentum"),
        repeat: Repeat::LastWins,
        applies_to: &["tag"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: None,
        shape: Some("mark name (ASCII alnum . _ - and / , at most 3 '/'-separated parts)"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some("Absent = no mark set."),
        short: "`tag` only. Sets a MARK — the stable second operand `gate --against` takes — in a file that is a SIBLING of the runs root (`vike_model::state_path::MARKS_SUBDIR`). Validated at PARSE time by `vike_model::runs::valid_mark_name`, whose message is prefixed with `--as `.",
        evidence: "crates/vike-cli/src/cmd/runs/tag.rs's `refuse_a_tag_that_writes_nothing` (the `valid_mark_name(name).map_err(|why| format!(\"--as {why}\"))` call) and `run_tag`; crates/vike-model/src/runs.rs's `valid_mark_name`",
        refusals: &[
            Refusal {
                on: "tag, empty name",
                message: "--as a mark name is required (for example `baseline/momentum`, or `prod`)",
            },
            Refusal {
                on: "tag, leading/trailing whitespace",
                message: "--as '{name}': a mark name may not begin or end with whitespace",
            },
            Refusal {
                on: "tag, more than 3 '/'-separated parts",
                message: "--as '{name}': a mark name may have at most 3 '/'-separated parts — a mark is a label, not a directory tree",
            },
            Refusal {
                on: "tag, an empty part",
                message: "--as '{name}': an empty part — a mark name may not start or end with '/' and may not contain '//'",
            },
            Refusal {
                on: "tag, a part starting with '.'",
                message: "--as '{name}': the part '{segment}' starts with '.' — that is a traversal ('..'), or a dot-entry every directory scan in this workspace skips, so the mark would be written and then be invisible",
            },
            Refusal {
                on: "tag, a disallowed character",
                message: "--as '{name}': the part '{segment}' contains '{bad}' — a mark name takes ASCII letters, digits, '.', '_', '-' and '/' only, because it becomes a file path on every platform this ships to",
            },
            Refusal {
                on: "ls, show, path, diff, gate, params, strategies",
                message: "--as does not apply to `backtest {sub}` — that flag WRITES a label, a note or a mark onto a stored run, which is what `tag` does",
            },
        ],
    },
    FlagRow {
        long: "--attribution",
        value: Value::None,
        value_name: None,
        sample: None,
        repeat: Repeat::Idempotent,
        applies_to: &[],
        refusal_reaches: &["ls", "show", "path", "tag", "diff", "gate", "params", "strategies"],
        status: Status::Unbuilt,
        roster_id: Some("unbuilt_renderers"),
        validated_by: "cli",
        profile_key: None,
        shape: Some("bare token — the guard matches on the flag token alone and consumes no value"),
        mode_scope: "local",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: None,
        short: "Refused by name on a reading sub-verb: the engine computes no per-source attribution at all, so this is missing DATA rather than a missing renderer.",
        evidence: "`crates/vike-cli/src/cmd/runs/show.rs`'s `refuse_an_unbuilt_renderer` (`\"--attribution\"` arm); roster `UNBUILT_RENDERERS`.",
        refusals: &[Refusal {
            on: "any use, on any of the eight reading sub-verbs",
            message: "--attribution is not available yet — it needs a per-source attribution the engine does not compute at all. What `show` can render today: --metrics, --trades, --config, --export trades|equity, --json, --out.",
        }],
    },
    FlagRow {
        long: "--breakdown",
        value: Value::None,
        value_name: None,
        sample: None,
        repeat: Repeat::Idempotent,
        applies_to: &[],
        refusal_reaches: &["ls", "show", "path", "tag", "diff", "gate", "params", "strategies"],
        status: Status::Unbuilt,
        roster_id: Some("unbuilt_renderers"),
        validated_by: "cli",
        profile_key: None,
        shape: Some("bare token — the guard matches on the flag token alone and consumes no value"),
        mode_scope: "local",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: None,
        short: "Refused by name on a reading sub-verb: a period breakdown needs per-bar returns, and the stored equity curve is DECIMATED once a run exceeds the sample cap, so a breakdown off it would disagree with report.json.",
        evidence: "`crates/vike-cli/src/cmd/runs/show.rs`'s `refuse_an_unbuilt_renderer` (`\"--breakdown\"` arm); roster `UNBUILT_RENDERERS`.",
        refusals: &[Refusal {
            on: "any use, on any of the eight reading sub-verbs",
            message: "--breakdown is not available yet — it needs the PER-BAR RETURNS. The stored equity curve is DECIMATED once a run exceeds the sample cap (series.json's `stride` above 1 means samples were dropped), so a period breakdown computed from it would disagree with report.json's own numbers. What `show` can render today: --metrics, --trades, --config, --export trades|equity, --json, --out.",
        }],
    },
    FlagRow {
        long: "--cash",
        value: Value::Required,
        value_name: Some("N"),
        sample: Some("10000"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: Some("engine.cash"),
        shape: Some("scalar (TOML-typed via parse_scalar)"),
        mode_scope: "any",
        default_kind: "far_side",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "NO default is supplied by either side. `implied_defaults`'s doc states it outright: a starting balance is a modelling input with no defensible default, so an omitted `--cash` is a FAR-SIDE missing-field refusal naming `engine.cash`.",
        ),
        short: "A `run`-only sugar flag (SUGAR row `engine.cash`, Shape::Scalar) included here because it has THREE different answers across this plane. ACCEPTED on `run`. On `params` it is on `PARAMS_REFUSED` and gets the run-flag sentence. On the other seven reading sub-verbs it is not a known token at all and falls into the generic unknown-option arm.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `SUGAR` (the `Sugar { flag: \"--cash\", key: \"engine.cash\", shape: Shape::Scalar }` row), `PARAMS_REFUSED`, `refuse_a_run_flag_on_params`, and `parse_read`'s final `other if other.starts_with(\"--\")` arm; `implied_defaults`'s ⚠ paragraph",
        refusals: &[
            Refusal {
                on: "params",
                message: "--cash does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            },
            Refusal {
                on: "ls, show, path, tag, diff, gate, strategies",
                message: "unknown option '--cash'",
            },
            Refusal {
                on: "run",
                message: "(ACCEPTED — no refusal. It pushes an Override with key `engine.cash`, value `parse_scalar(raw)`, origin `Origin::Sugar(\"--cash\")`.)",
            },
        ],
    },
    FlagRow {
        long: "--changed-only",
        value: Value::None,
        value_name: None,
        sample: None,
        repeat: Repeat::Idempotent,
        applies_to: &["diff"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: None,
        shape: Some("bool"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "It IS the default; the flag exists so a script that says what it means is not refused.",
        ),
        short: "`diff` only, and a deliberate NO-OP: `ReadArgs::changed_only` is set by the parser and read by NOTHING in run_diff. It is accepted purely so an explicit script is not refused.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `ReadArgs::changed_only` (its doc: \"the DEFAULT, accepted so a script that says what it means is not refused\"); crates/vike-cli/src/cmd/runs/diff.rs's `run_diff` never reads the field",
        refusals: &[
            Refusal {
                on: "ls, show, path, tag, gate, params, strategies",
                message: "--changed-only does not apply to `backtest {sub}` — that flag shapes a two-run COMPARISON, which is what `diff` renders",
            },
            Refusal {
                on: "(any reading sub-verb, with a value)",
                message: "--changed-only takes no value",
            },
        ],
    },
    FlagRow {
        long: "--cols",
        value: Value::Required,
        value_name: Some("a,b,c"),
        sample: Some("run_id,kind,sharpe"),
        repeat: Repeat::LastWins,
        applies_to: &["ls"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: Some("ls_field_vocabulary"),
        validated_by: "cli",
        profile_key: Some("ls_default_cols"),
        shape: Some("comma list of field names"),
        mode_scope: "any",
        default_kind: "implied",
        default_value: Some("run_id,kind,started_at,report"),
        default_conditional_on: None,
        default_reason: Some(
            "`ls::DEFAULT_COLS` — manifest-only, so a bare `ls` opens no report.json at all.",
        ),
        short: "`ls` only. Split on ',', each element trimmed, EMPTY elements dropped — so `--cols 'a,,b'` is two columns, not three, and `--cols ,` is refused as no column names. An unknown column name is NOT refused: it renders as a column of '-'.",
        evidence: "crates/vike-cli/src/cmd/runs/ls.rs's `run_ls` (the `a.cols` split) and `DEFAULT_COLS`",
        refusals: &[
            Refusal {
                on: "ls, when every element is empty after trimming",
                message: "--cols was given no column names",
            },
            Refusal {
                on: "show, path, tag, diff, gate, params, strategies",
                message: "--cols does not apply to `backtest {sub}` — that flag narrows, orders or shapes a LISTING, and `ls` is the listing",
            },
        ],
    },
    FlagRow {
        long: "--config",
        value: Value::None,
        value_name: None,
        sample: None,
        repeat: Repeat::Idempotent,
        applies_to: &["show"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: None,
        shape: Some("bool"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some("Same `all` rule as --metrics."),
        short: "`show` only. Renders the run's RunConfig (the profile path as the operator spelled it, plus its name) and the resolved config.toml when the producer wrote one — saying so when it is absent.",
        evidence: "crates/vike-cli/src/cmd/runs/show.rs's `show_text` (the `config || all` branch) and that module's doc `⚠ --config says what is NOT recorded`",
        refusals: &[
            Refusal {
                on: "ls, path, tag, diff, gate, params, strategies",
                message: "--config does not apply to `backtest {sub}` — that flag selects a SECTION of one stored run, which is what `show` renders",
            },
            Refusal {
                on: "(any reading sub-verb, with a value)",
                message: "--config takes no value",
            },
        ],
    },
    FlagRow {
        long: "--drawdowns",
        value: Value::None,
        value_name: None,
        sample: None,
        repeat: Repeat::Idempotent,
        applies_to: &[],
        refusal_reaches: &["ls", "show", "path", "tag", "diff", "gate", "params", "strategies"],
        status: Status::Unbuilt,
        roster_id: Some("unbuilt_renderers"),
        validated_by: "cli",
        profile_key: None,
        shape: Some(
            "bare token IN THE CODE. §6.2 spells it `--drawdowns N`, but the guard fires on the flag token before any value is read: `--drawdowns=5` matches on `--drawdowns` and refuses, and in `--drawdowns 5` the `5` is never reached because the guard returns Err immediately.",
        ),
        mode_scope: "local",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: None,
        short: "Refused by name on a reading sub-verb: the drawdown TABLE is the missing renderer, not the data — the equity curve is in the run record and `--export equity` hands it over today.",
        evidence: "`crates/vike-cli/src/cmd/runs/show.rs`'s `refuse_an_unbuilt_renderer` (`\"--drawdowns\"` arm); roster `UNBUILT_RENDERERS`.",
        refusals: &[Refusal {
            on: "any use, on any of the eight reading sub-verbs",
            message: "--drawdowns is not available yet — it needs a drawdown-table renderer, which nothing in this tree has built. The EQUITY CURVE it would read is in the run record now (series.json), so this is the renderer that is missing rather than the data — `--export equity` hands you the curve today. What `show` can render today: --metrics, --trades, --config, --export trades|equity, --json, --out.",
        }],
    },
    FlagRow {
        long: "--engine",
        value: Value::Required,
        value_name: Some("PATH"),
        sample: Some("/opt/backtest"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: None,
        shape: Some("a path to the standalone engine binary, used as the program to spawn"),
        mode_scope: "local",
        default_kind: "implied",
        default_value: Some(
            "<project>/bin/backtest[EXE] → <exe_dir>/backtest[EXE] → <project>/bin/vike[EXE] with lead arg `backtest` → <exe_dir>/vike[EXE] with lead arg `backtest` → bare `backtest` on PATH",
        ),
        default_conditional_on: None,
        default_reason: Some(
            "`crate::cmd::engine`'s `locate(explicit, project_root)`: an explicit path short-circuits the whole ladder and is deliberately NOT probed for existence (a named path that is not there must fail with the name the operator typed), and is never treated as a multicall. Otherwise the four-rung standalone-then-multicall search runs, with bare `backtest` on PATH as the last word.",
        ),
        short: "Name the standalone engine outright instead of searching for it. Local-arm only; never probed for existence.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `Args::engine`, `parse_run_args`'s remote-arm refusal loop and `execute_local`; crates/vike-cli/src/cmd/engine.rs's `locate`, `ENGINE_BIN` and `cannot_spawn`",
        refusals: &[
            Refusal {
                on: "⚠ typed WITHOUT --local, i.e. on the remote arm (CLI, exit 2). Same one-line template as --store.",
                message: "--engine applies to --local only — a remote run reads the SERVER's store",
            },
            Refusal {
                on: "the named program cannot be spawned (CONNECT rung; `{}` is the engine display form, `{e}` the io::Error, `MISSING_HINT` a constant in crates/vike-cli/src/cmd/engine.rs that this area did not transcribe)",
                message: "cannot run the backtest engine ({engine}): {e}\n{MISSING_HINT}",
            },
            Refusal { on: "written with no value", message: "--engine requires a value" },
        ],
    },
    FlagRow {
        long: "--euler-depth",
        value: Value::Required,
        value_name: Some("N"),
        sample: Some("2"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "engine",
        profile_key: None,
        shape: Some("a token, FORWARDED VERBATIM; the engine wants an integer in 0..=16"),
        mode_scope: "any",
        default_kind: "far_side",
        default_value: Some("3"),
        default_conditional_on: None,
        default_reason: Some(
            "CONFIRMED forwarded verbatim (same three sites as --trials). Default applied only under euler, by `search_select`'s `parse_euler_depth` → `EulerConfig::DEFAULT_MAX_DEPTH` = 3; the hard cap is `EulerConfig::MAX_DEPTH_CAP` = 16 and 0 is legal (coarse-grid-only).",
        ),
        short: "euler's successive-halving depth. Forwarded as a token; range-checked (0..=16) by the engine, never clamped silently.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `Args::euler_depth` and `parse_run_args`'s `\"--euler-depth\"` arm; crates/vike-backtest/src/harness/search_select.rs's `parse_euler_depth`; crates/vike-backtest/src/search.rs's `EulerConfig::DEFAULT_MAX_DEPTH` and `EulerConfig::MAX_DEPTH_CAP`",
        refusals: &[
            Refusal {
                on: "--euler-depth under any method but euler (from METHOD_KNOBS, both routes)",
                message: "--euler-depth is a euler flag, but this run selected the \"grid\" optimizer. It was silently discarded before; it is refused now, because a knob that configures a search you did not select cannot do what it says",
            },
            Refusal {
                on: "a value past the cap, under euler (`{d}` is the parsed value, the second hole is EulerConfig::MAX_DEPTH_CAP)",
                message: "--euler-depth 99 is past the cap of 16 — it was silently clamped before, which made the budget line report a depth you did not ask for",
            },
            Refusal {
                on: "a non-integer value, under euler",
                message: "invalid --euler-depth \"abc\" (expected an integer)",
            },
            Refusal {
                on: "written with no value, on the engine's argv path",
                message: "--euler-depth was written with no value (expected an integer). A trailing `--euler-depth` read as ABSENT before, which silently ran the default instead of refusing — write `--euler-depth <value>`",
            },
        ],
    },
    FlagRow {
        long: "--export",
        value: Value::Required,
        value_name: Some("trades|equity"),
        sample: Some("trades"),
        repeat: Repeat::LastWins,
        applies_to: &["show"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: Some("show_export_values"),
        validated_by: "cli",
        profile_key: None,
        shape: Some("one value from a roster"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some("Absent = the ordinary rendering."),
        short: "`show` only. REPLACES the rendering entirely with the raw stored document (pretty-printed JSON), and composes with `--out` and with nothing else. The value is trimmed. A COMMA LIST is refused by name. `fills` is refused by NAME with its own sentence rather than by refusing the flag.",
        evidence: "crates/vike-cli/src/cmd/runs/show.rs's `export_document` and `EXPORTABLE`; `run_show`'s `match &a.export`",
        refusals: &[
            Refusal {
                on: "value `fills` (Usage, rung 2) — refused at EXECUTION time in `export_document`, not at parse time",
                message: "--export fills is not available — fills are not stored at ANY size. A backtest computes them, consumes them into report.json's scalars and drops them; only the trade LEDGER and the equity curve survive. `--export trades` and `--export equity` both work today.",
            },
            Refusal {
                on: "any value containing a comma, e.g. `trades,equity` (Usage, rung 2). Template: \"--export '{value}': one value at a time. …\" where {value} is the trimmed spec as typed",
                message: "--export 'trades,equity': one value at a time. Each export is a single document and `--out` names a single file, so a list would either be truncated to its first value or written as two documents nothing can parse. Run `show` once per value.",
            },
            Refusal {
                on: "any other unrecognised value (Usage, rung 2). Template: \"--export '{other}' is not a value this verb knows — §6.2's set is trades, equity and fills, of which {list} are stored (fills are not: see `--export fills`)\" where {other} is the trimmed value and {list} is `EXPORTABLE.join(\" and \")` = \"trades and equity\"",
                message: "--export 'nonsense' is not a value this verb knows — §6.2's set is trades, equity and fills, of which trades and equity are stored (fills are not: see `--export fills`)",
            },
            Refusal {
                on: "`--export trades` on a run whose producer wrote no trades.json (Failed, rung 1). Template holes: {run_id} = the run's directory name, {file} = `vike_model::runs::TRADES_FILE` = \"trades.json\"",
                message: "a-1-0 wrote no trades.json — this producer keeps no trade ledger (a study, or a run minted before the run artifact existed)",
            },
            Refusal {
                on: "`--export equity` on a run whose producer wrote no series.json (Failed, rung 1). Template holes: {run_id} = the run's directory name, {file} = `vike_model::runs::SERIES_FILE` = \"series.json\"",
                message: "a-1-0 wrote no series.json — this producer keeps no equity curve",
            },
            Refusal {
                on: "any other RunReadError on either value (Failed, rung 1)",
                message: "<the `RunReadError`'s own Display, passed through verbatim by `CliError::failed(other.to_string())`>",
            },
            Refusal {
                on: "used on any sub-verb other than `show` (Usage, rung 2), by `refuse_foreign_read_flags`'s `show_only` roster. Template: \"{flag} does not apply to `backtest {sub}` — {why}\"",
                message: "--export does not apply to `backtest ls` — that flag selects a SECTION of one stored run, which is what `show` renders",
            },
        ],
    },
    FlagRow {
        long: "--fail-if",
        value: Value::Required,
        value_name: Some("EXPR"),
        sample: Some("sharpe:-5%,max_dd:+10%"),
        repeat: Repeat::LastWins,
        applies_to: &["gate"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: None,
        shape: Some("criteria expression"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some("No default — REQUIRED on `gate`."),
        short: "`gate` only, and REQUIRED. Grammar: EXPR := CRITERION (',' CRITERION)* ; CRITERION := METRIC ':' SIGN NUMBER ['%'] ; SIGN := '+' | '-'. The sign says which direction is BAD and is REQUIRED; '%' makes the tolerance a share of |baseline|. RELATIVE only — there is no absolute form. Parsed TWICE: once by `refuse_an_ungateable_line` (before any directory is opened) and again in `run_gate`.",
        evidence: "crates/vike-cli/src/cmd/runs/gate.rs's `refuse_an_ungateable_line`; crates/vike-cli/src/cmd/runs/failif.rs's `parse_fail_if` and that module's grammar block",
        refusals: &[
            Refusal {
                on: "the whole expression is empty or whitespace",
                message: "--fail-if takes at least one criterion, e.g. `sharpe:-5%,max_dd:+10%` — METRIC, a colon, a SIGN saying which direction is bad, the tolerance, and an optional `%`",
            },
            Refusal {
                on: "a term between two commas is empty",
                message: "--fail-if: an empty criterion — terms are joined by a single comma, e.g. `sharpe:-5%,max_dd:+10%`",
            },
            Refusal {
                on: "the colon is missing (format! template; {term} is the trimmed term)",
                message: "--fail-if `{term}`: a criterion is METRIC:SIGN NUMBER[%], e.g. `sharpe:-5%` — the colon is missing",
            },
            Refusal {
                on: "the metric before the colon is empty ({term} = the trimmed term)",
                message: "--fail-if `{term}`: the metric before the colon is empty",
            },
            Refusal {
                on: "no leading + or - on the tolerance ({term} = the trimmed term, {metric} = the trimmed metric)",
                message: "--fail-if `{term}`: the tolerance needs a SIGN saying which direction is bad — `-` fails on a FALL (`{metric}:-5%`), `+` fails on a RISE (`{metric}:+5%`). Without one this gate would check a side of the number you did not choose.",
            },
            Refusal {
                on: "the magnitude does not parse as f64 ({term} = the trimmed term, {number} = the text after the sign with any trailing % stripped, NOT trimmed)",
                message: "--fail-if `{term}`: `{number}` is not a number — the tolerance is the MAGNITUDE",
            },
            Refusal {
                on: "the magnitude parses but is not finite, e.g. `sharpe:+inf` ({term}, {number} as above)",
                message: "--fail-if `{term}`: `{number}` is not a finite tolerance",
            },
            Refusal {
                on: "the magnitude is negative, e.g. `sharpe:--5%` ({term} = the trimmed term, {metric} = the trimmed metric)",
                message: "--fail-if `{term}`: the tolerance is negative — the DIRECTION is the sign's job and the number is the magnitude, so `{metric}:-5%` is what `{term}` was probably meant to be",
            },
            Refusal {
                on: "two criteria resolve to the SAME report key — including an alias pair such as `max_dd:+5%,max_drawdown:+10%` ({prev} and {c} are the two criteria re-rendered by Criterion's Display, {key} is the resolved JSON key)",
                message: "`{prev}` and `{c}` both judge `{key}` — name each metric once; a silently-dropped criterion is a gate checking something other than what it was given",
            },
        ],
    },
    FlagRow {
        long: "--fee",
        value: Value::Required,
        value_name: Some("RATE"),
        sample: Some("0.001"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "engine",
        profile_key: Some("engine.fee_rate"),
        shape: Some("Scalar"),
        mode_scope: "any",
        default_kind: "schema",
        default_value: Some("0.0"),
        default_conditional_on: None,
        default_reason: Some(
            "`EngineCfg::fee_rate` is `#[serde(default)] f64` in crates/vike-backtest/src/harness/profile.rs, so an absent value is 0.0. The CLI supplies nothing.",
        ),
        short: "Sugar for engine.fee_rate through parse_scalar; a non-numeric value becomes a TOML string and is refused far-side, not here.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's SUGAR (row `--fee` -> `engine.fee_rate`, Shape::Scalar); crates/vike-cli/src/cmd/backtest.rs's parse_scalar",
        refusals: &[],
    },
    FlagRow {
        long: "--from",
        value: Value::Required,
        value_name: Some("DATE"),
        sample: Some("2026-01-01T00"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "engine",
        profile_key: Some("data.from"),
        shape: Some("Str"),
        mode_scope: "any",
        default_kind: "far_side",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "`DataCfg::from` carries NO serde default in crates/vike-backtest/src/harness/profile.rs, so an absent value is a far-side missing-field refusal. The CLI implies nothing.",
        ),
        short: "Sugar for data.from. Shape::Str is REQUIRED not a preference: a bare epoch-ms stays a string, so --from 0 works while --set data.from=0 is a far-side type error.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's SUGAR and its Shape::Str doc; pinned by crates/vike-cli/src/cmd/backtest.rs's a_bare_epoch_ms_date_stays_a_string_through_the_sugar_flag",
        refusals: &[],
    },
    FlagRow {
        long: "--help",
        value: Value::None,
        value_name: None,
        sample: None,
        repeat: Repeat::Idempotent,
        applies_to: &["ls", "show", "path", "tag", "diff", "gate", "params", "strategies"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: None,
        shape: Some("bool"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: None,
        short: "`-h` or `--help`, matched inside the shared reading flag loop. Also FALLS THROUGH — no refusal array names it. It short-circuits through the Err channel with `args::HELP_SENTINEL` (\"help requested\"), which `args::exit_for_parse_error` turns back into the USAGE on stdout and exit 0.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `parse_read` (the `\"-h\" | \"--help\"` arm calls `args::help_requested`); crates/vike-cli/src/cmd/args.rs's `HELP_SENTINEL` and `exit_for_parse_error`",
        refusals: &[Refusal {
            on: "`--version` / `-V` typed inside the backtest plane",
            message: "unknown argument: --version",
        }],
    },
    FlagRow {
        long: "--html",
        value: Value::None,
        value_name: None,
        sample: None,
        repeat: Repeat::Idempotent,
        applies_to: &[],
        refusal_reaches: &["ls", "show", "path", "tag", "diff", "gate", "params", "strategies"],
        status: Status::Unbuilt,
        roster_id: Some("unbuilt_renderers"),
        validated_by: "cli",
        profile_key: None,
        shape: Some(
            "bare token — the guard matches on the flag token alone and consumes no value, so `--html` and `--html=x` both refuse identically",
        ),
        mode_scope: "local",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: None,
        short: "Refused by name on a reading sub-verb: a §6.2 tearsheet flag nothing in this tree renders. Refused by NAME (not \"unknown option\") on EVERY reading sub-verb, because the guard sits in the one shared flag loop with no sub-verb condition.",
        evidence: "`crates/vike-cli/src/cmd/runs/show.rs`'s `UNBUILT_RENDERERS` and `refuse_an_unbuilt_renderer`; the guard arm in `crates/vike-cli/src/cmd/backtest.rs`'s `parse_read`.",
        refusals: &[Refusal {
            on: "any use, on any of the eight reading sub-verbs",
            message: "--html is not available yet — it needs a tearsheet renderer, which nothing in this tree has built. What `show` can render today: --metrics, --trades, --config, --export trades|equity, --json, --out.",
        }],
    },
    FlagRow {
        long: "--interval",
        value: Value::Required,
        value_name: Some("1h"),
        sample: Some("1h"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: Some("data.interval"),
        shape: Some("Str"),
        mode_scope: "any",
        default_kind: "schema",
        default_value: Some("1d"),
        default_conditional_on: None,
        default_reason: Some(
            "`DataCfg::interval` is `#[serde(default = \"default_interval\")]` and crates/vike-backtest/src/harness/profile.rs's default_interval returns \"1d\". The CLI supplies nothing.",
        ),
        short: "Sugar for data.interval, plus the ONE grammar check inside the match body: the trimmed value must parse through vike_model::time::interval_ms.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's parse_run_args (the `s.flag == \"--interval\"` special case inside the Shape::Str arm); crates/vike-model/src/time.rs's interval_ms",
        refusals: &[Refusal {
            on: "vike_model::time::interval_ms returns None — fewer than 2 chars, a non-digit count, or an unknown trailing unit. NOTE: a ZERO count (`0m`) DOES parse and is NOT refused here.",
            message: "--interval {v:?} is not a valid interval — a digit count and one unit of s|m|h|d (e.g. 30s, 5m, 1h, 1d)",
        }],
    },
    FlagRow {
        long: "--json",
        value: Value::None,
        value_name: None,
        sample: None,
        repeat: Repeat::Idempotent,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: None,
        shape: Some("bare boolean switch"),
        mode_scope: "any",
        default_kind: "none",
        default_value: Some("false"),
        default_conditional_on: None,
        default_reason: Some(
            "Absent: the remote arm pretty-prints the report (or renders the ranked table for a search, or the stitched OOS table for a walk-forward) and the local arm omits `--json` from the child argv. Present: the remote arm prints the server's report JSON VERBATIM and the local arm pushes `--json` into the child argv.",
        ),
        short: "Print the report JSON exactly as the server emitted it, instead of a human rendering. On --local it is forwarded to the spawned engine.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `Args::json`, `parse_run_args`'s `\"--json\"` arm, `execute` and `execute_local`",
        refusals: &[Refusal {
            on: "an inline value (`--json=1`)",
            message: "--json takes no value",
        }],
    },
    FlagRow {
        long: "--kind",
        value: Value::Required,
        value_name: Some("bar|tick"),
        sample: Some("bar"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: Some("data_kinds"),
        validated_by: "cli",
        profile_key: Some("data.kind"),
        shape: Some("Str"),
        mode_scope: "any",
        default_kind: "implied",
        default_value: Some("bar"),
        default_conditional_on: None,
        default_reason: Some("no --kind and no profile said otherwise"),
        short: "Sugar for data.kind, plus the second in-match special case: the trimmed value goes through one_of against DATA_KINDS. It is the key implied_defaults always supplies.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's parse_run_args (the `s.flag == \"--kind\"` special case calls one_of with DATA_KINDS); crates/vike-cli/src/cmd/backtest.rs's implied_defaults",
        refusals: &[Refusal {
            on: "a value outside DATA_KINDS (exact, case-sensitive)",
            message: "--kind must be bar|tick, got {value:?}",
        }],
    },
    FlagRow {
        long: "--limit",
        value: Value::Required,
        value_name: Some("N"),
        sample: Some("20"),
        repeat: Repeat::LastWins,
        applies_to: &["ls"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: None,
        shape: Some("usize"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some("Absent = no truncation."),
        short: "`ls` only. Parsed as `usize` at RENDER time (not in the flag loop), then `runs.truncate(n)`. A negative or non-integer value is a usage refusal.",
        evidence: "crates/vike-cli/src/cmd/runs/ls.rs's `run_ls` (the `a.limit` parse + `truncate`)",
        refusals: &[
            Refusal {
                on: "ls, non-usize value",
                message: "--limit '{n}' is not a whole number of rows",
            },
            Refusal {
                on: "show, path, tag, diff, gate, params, strategies",
                message: "--limit does not apply to `backtest {sub}` — that flag narrows, orders or shapes a LISTING, and `ls` is the listing",
            },
        ],
    },
    FlagRow {
        long: "--local",
        value: Value::None,
        value_name: None,
        sample: None,
        repeat: Repeat::Idempotent,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: None,
        shape: Some("bare boolean switch"),
        mode_scope: "any",
        default_kind: "none",
        default_value: Some("false"),
        default_conditional_on: None,
        default_reason: Some(
            "Absent = the REMOTE arm: the profile text is shipped to a compute daemon. Present = `execute_local` spawns the standalone `backtest` engine on this machine instead.",
        ),
        short: "Run on THIS machine by spawning the standalone backtest engine instead of shipping the profile to a compute daemon.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `Args::local`, `parse_run_args`'s `\"--local\"` arm, `execute` and `execute_local`",
        refusals: &[
            Refusal {
                on: "--local combined with --addr (CLI, exit 2). The literal carries a real newline; the line continuations strip their own leading whitespace, so the second line begins at `drop one:`.",
                message: "--addr names a remote backtest daemon, so it cannot be combined with --local\ndrop one: --local runs the engine on this machine, --addr ships the profile to a server",
            },
            Refusal {
                on: "--local on a profile that declares a [walkforward] table (CLI, exit 2). One real newline before `Drop --local`.",
                message: "--local cannot run a walk-forward: this profile declares a [walkforward] table, and the standalone engine has ONE profile path — it branches on the [paramscan] grid and nothing else, so neither walk-forward driver is reachable from any binary and there is nothing local to spawn.\nDrop --local to run it on the compute daemon, or drop the [walkforward] table to backtest this profile here.",
            },
            Refusal { on: "an inline value (`--local=1`)", message: "--local takes no value" },
            Refusal {
                on: "--local when the profile had to be STAGED and there is no project above the working directory (exit 1). `{}` is filled by one of the two sentences below.",
                message: "--local runs the engine on this machine from a profile FILE, and there is no project above the working directory to stage one in.\n{staging reason}\nRun inside your project (or set $VIKE_SETTINGS_DIR), pass an already-merged profile to --profile, or write one first with --write-profile <path>",
            },
        ],
    },
    FlagRow {
        long: "--md",
        value: Value::None,
        value_name: None,
        sample: None,
        repeat: Repeat::Idempotent,
        applies_to: &["diff"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: None,
        shape: Some("bool"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some("Absent = the human table."),
        short: "`diff` only. Renders the same rows as a Markdown table. ⚠ `--json` WINS over it — run_diff tests `a.json` first, so `--json --md` emits JSON.",
        evidence: "crates/vike-cli/src/cmd/runs/diff.rs's `run_diff` (the `if a.json { … } else if a.md { … }` ladder)",
        refusals: &[
            Refusal {
                on: "ls, show, path, tag, gate, params, strategies",
                message: "--md does not apply to `backtest {sub}` — that flag shapes a two-run COMPARISON, which is what `diff` renders",
            },
            Refusal { on: "(any reading sub-verb, with a value)", message: "--md takes no value" },
        ],
    },
    FlagRow {
        long: "--metrics",
        value: Value::None,
        value_name: None,
        sample: None,
        repeat: Repeat::Idempotent,
        applies_to: &["show"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: None,
        shape: Some("bool"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "`show_text` computes `all = !metrics && !trades && !config`, so NO section flag means EVERY section renders. The three section flags NARROW, they do not add.",
        ),
        short: "`show` only. Renders the report.json metrics section. Has no effect under `--json` (`show_json` takes only `a.trades`) and none under `--export` (the export replaces the rendering entirely).",
        evidence: "crates/vike-cli/src/cmd/runs/show.rs's `show_text` and `run_show`; crates/vike-cli/src/cmd/backtest.rs's `refuse_foreign_read_flags` (`show_only`)",
        refusals: &[
            Refusal {
                on: "ls, path, tag, diff, gate, params, strategies",
                message: "--metrics does not apply to `backtest {sub}` — that flag selects a SECTION of one stored run, which is what `show` renders",
            },
            Refusal {
                on: "(any reading sub-verb, with a value)",
                message: "--metrics takes no value",
            },
        ],
    },
    FlagRow {
        long: "--note",
        value: Value::Required,
        value_name: Some("TEXT"),
        sample: Some("fee model fix"),
        repeat: Repeat::LastWins,
        applies_to: &["tag"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: None,
        shape: Some("string"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some("Absent = no note written."),
        short: "`tag` only. APPENDED, never replacing. ⚠ When `--as` is also given the SAME note is written TWICE on disk — into the run's sidecar and into the mark — deliberately, so the reason travels with the pointer.",
        evidence: "crates/vike-cli/src/cmd/runs/tag.rs's `run_tag` (passes `a.note` to both `add_tags` and `write_mark`) and that function's ⚠ doc",
        refusals: &[Refusal {
            on: "ls, show, path, diff, gate, params, strategies",
            message: "--note does not apply to `backtest {sub}` — that flag WRITES a label, a note or a mark onto a stored run, which is what `tag` does",
        }],
    },
    FlagRow {
        long: "--optimizer",
        value: Value::Required,
        value_name: Some("method"),
        sample: Some("grid"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: Some("optimizers"),
        validated_by: "cli",
        profile_key: None,
        shape: Some(
            "one of the OPTIMIZERS roster, spelling-checked here and RESOLVED by whichever side runs",
        ),
        mode_scope: "any",
        default_kind: "far_side",
        default_value: Some("grid"),
        default_conditional_on: None,
        default_reason: Some(
            "This crate substitutes NO default — an absent --optimizer is forwarded as `None` on BOTH routes and the side that RUNS resolves it against `vike_datahub_client::DEFAULT_SEARCH_METHOD` (\"grid\"). `DEFAULT_OPTIMIZER` in vike-cli is `#[cfg(test)]`-gated and exists only to build sample argv in that file's own tests.",
        ),
        short: "The parameter-search METHOD (grid, euler, tpe, genetic). Spelling-checked locally against the protocol roster; meaning resolved by the engine or the compute server.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `OPTIMIZERS`, `parse_run_args`'s `\"--optimizer\"` arm and `one_of`; crates/vike-cli/src/cmd/backtest.rs's `DEFAULT_OPTIMIZER` (cfg(test)); crates/vike-datahub-client/src/proto.rs's `SEARCH_METHODS` and `DEFAULT_SEARCH_METHOD`; crates/vike-backtest/src/harness/search_select.rs's `resolve`",
        refusals: &[
            Refusal {
                on: "a value outside OPTIMIZERS (CLI, before any dial or spawn; exit 2)",
                message: "--optimizer must be grid|euler|tpe|genetic, got \"bogus\"",
            },
            Refusal {
                on: "typed on a profile that declares a [walkforward] table (CLI, exit 2)",
                message: "--optimizer configures nothing on a walk-forward run: the wire verb it goes over carries the profile TOML and nothing else. A window's ranking is [walkforward].rank_by, in the profile, read by the one parser that runs it — and what each window searches is that same [walkforward] table's business, never a flag on this side.",
            },
            Refusal {
                on: "--optimizer genetic with no --seed (engine/server, via search_select::require_seed)",
                message: "--optimizer genetic requires --seed <u64>. It is not defaulted, deliberately: a genetic search reports ONE sample of a distribution, and a seed nobody typed is a constant the result silently depends on — write `--seed 7` (any u64) and the run is reproducible from your own shell history",
            },
            Refusal {
                on: "--local, on a profile with no [paramscan] table (the spawned engine, exit 2; `{asked}` is the space-joined subset of --optimizer --euler-depth --trials --seed --keep-trials --resume that argv actually carried, `{profile_path:?}` the Debug-quoted profile path)",
                message: "backtest: {asked} names a parameter SEARCH, but {profile_path:?} has no [paramscan] table — there is nothing to search. Add one, or drop the search flags",
            },
            Refusal {
                on: "remote, on a profile with no [paramscan] table (compute server Response::Error; the literal contains a real newline before `fast = …`)",
                message: "profile has no [paramscan] table — a parameter search needs a grid, e.g. `[paramscan]\nfast = [5, 10, 15]`",
            },
            Refusal {
                on: "any non-grid method against a daemon that does not advertise `search_method` — refused locally, NOTHING sent (`{:?}` renders the daemon's advertised feature list)",
                message: "backtest daemon does not advertise `search_method` (advertised: {features:?}) — nothing was sent. A search METHOD (--optimizer/--euler-depth/--trials/--seed) and --rank-by multi need a newer `vike-backend backtest --addr`; this verb is capability-negotiated, not version-gated. An older daemon would DROP the field, run the exhaustive grid and report success, which is the silent downgrade this refusal exists to prevent — upgrade the daemon, or drop the flag to search the grid on the server.",
            },
            Refusal {
                on: "written with no value (trailing `--optimizer`, or `--optimizer` followed by another `--` token)",
                message: "--optimizer requires a value, but the next argument is another flag (--json)",
            },
        ],
    },
    FlagRow {
        long: "--out",
        value: Value::Required,
        value_name: Some("FILE"),
        sample: Some("runs.txt"),
        repeat: Repeat::LastWins,
        applies_to: &["ls", "show"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: None,
        shape: Some("path"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some("Absent = the rendered text goes to stdout."),
        short: "ONE OF THE TWO DOUBLE-OWNED FLAGS. Owners: `ls` (writes the table or the --json document) and `show` (writes the rendered run, or the raw --export document). Refused on the other six by a NAMED rule, not by a roster: `gate` and `diff` are deliberately excluded because their single clean stream is what `>` already handles. The write itself is `std::fs::write(file, format!(\"{text}\\n\"))` — note the appended newline — and an existing path is OVERWRITTEN (no refusal).",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `refuse_foreign_read_flags` (the `!matches!(a.sub, ReadSub::Ls | ReadSub::Show) && a.out.is_some()` rule); crates/vike-cli/src/cmd/runs/ls.rs's `run_ls`; crates/vike-cli/src/cmd/runs/show.rs's `run_show`",
        refusals: &[
            Refusal {
                on: "path, tag, diff, gate, params, strategies",
                message: "--out names a FILE to write a rendered document to, and `backtest {sub}` prints a line the shell can already redirect",
            },
            Refusal {
                on: "ls or show, when the file cannot be written",
                message: "cannot write {file}: {io_error}",
            },
            Refusal { on: "(any reading sub-verb, no value)", message: "--out requires a value" },
        ],
    },
    FlagRow {
        long: "--param",
        value: Value::Required,
        value_name: Some("k=v"),
        sample: Some("size=1.5"),
        repeat: Repeat::Accumulate,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: Some("strategy.params.<k>"),
        shape: None,
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "Writes one knob under [strategy.params]; there is nothing to default. It is NOT a SUGAR row — it has its own match arm — but it stamps Origin::Sugar(\"--param\"), so --show-effective renders its origin as `--param`.",
        ),
        short: "Sugar for --set strategy.params.<k>=<v>, value typed by parse_scalar. NO schema validation is possible on either side: StrategyCfg::params is an untyped toml::Value, so a typo is a silent no-op.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's parse_run_args (the `\"--param\"` arm); crates/vike-cli/src/cmd/backtest.rs's looks_like_a_search_axis",
        refusals: &[
            Refusal {
                on: "the value carries no `=`",
                message: "--param takes k=v, got {pair:?} (e.g. --param size=1.5)",
            },
            Refusal {
                on: "the key is empty or whitespace (refused HERE so the message names --param rather than the --set grammar)",
                message: "--param takes k=v with a non-empty key, got {pair:?} (e.g. --param size=1.5)",
            },
            Refusal {
                on: "the key contains a `.`",
                message: "--param {key}: [strategy.params] is a FLAT knob table — use --set strategy.params.{key}=… if you really mean a nested key",
            },
            Refusal {
                on: "the value looks like a search axis — it starts with `[`, or it is three colon-separated parts that all parse as f64",
                message: "--param {key}={raw}: a RANGE declares a search axis, which this command cannot build yet — declare it as a [paramscan] table in a --profile, or pass a single value. (--set strategy.params.{key}={raw} sets it as a literal value if that is what you meant.)",
            },
        ],
    },
    FlagRow {
        long: "--preset",
        value: Value::Required,
        value_name: Some("<p.toml>"),
        sample: Some("fast.toml"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: None,
        shape: None,
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "Writes no single key — it is a CLIENT-SIDE rewrite that merges a flat knob table into [strategy.params], last-wins over anything the profile already set there.",
        ),
        short: "A flat preset TOML merged into [strategy.params], applied BEFORE --script. Repeating it silently LAST-WINS.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's merge_preset_params; crates/vike-cli/src/cmd/backtest.rs's parse_run_args (the `\"--preset\"` arm)",
        refusals: &[
            Refusal {
                on: "the preset wraps its knobs in a [params] table (exactly one key, and it is a table named `params`) — the message is prefixed `preset {preset_path}: ` by execute",
                message: "it wraps its knobs in a [params] table, so the strategy would receive one parameter called 'params' that nothing reads and every knob would keep its default. A preset IS the params table: delete the [params] header and leave the keys at the top level",
            },
            Refusal {
                on: "the preset defines `src`",
                message: "it defines 'src', which is the strategy's SOURCE rather than one of its knobs — that is what `--script` is for. Delete the 'src' key",
            },
            Refusal { on: "the preset is not valid TOML", message: "not valid TOML: {e}" },
            Refusal {
                on: "the preset is TOML but not a table",
                message: "a preset must be a table of parameters",
            },
            Refusal {
                on: "the file cannot be read",
                message: "cannot read preset {preset_path}: {e}",
            },
        ],
    },
    FlagRow {
        long: "--profile",
        value: Value::Required,
        value_name: Some("<run.toml>"),
        sample: Some("run.toml"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: None,
        shape: None,
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "OPTIONAL since stage 2 — it supplies a BASE document the overrides are applied onto, not a profile key. Absent, the base is an empty TOML table.",
        ),
        short: "Optional BASE profile file; flags override it. Repeating it silently LAST-WINS (plain Option assignment, no test pins it).",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's parse_run_args (the `\"--profile\"` arm assigns `profile_path = Some(flags.value(&flag, inline)?)`); crates/vike-cli/src/cmd/backtest.rs's execute reads it into `base` and hands it to build_profile_toml",
        refusals: &[
            Refusal {
                on: "a trailing --profile with nothing after it",
                message: "{flag} requires a value",
            },
            Refusal {
                on: "the next token starts with `--`",
                message: "{flag} requires a value, but the next argument is another flag ({found})",
            },
            Refusal {
                on: "the file cannot be read (in execute, exit rung 1)",
                message: "cannot read profile {path}: {e}",
            },
            Refusal {
                on: "the file is not valid TOML (in build_profile_toml, lifted onto the USAGE rung by CliError::usage)",
                message: "profile is not valid TOML: {e}",
            },
        ],
    },
    FlagRow {
        long: "--rank-by",
        value: Value::Required,
        value_name: Some("METRIC"),
        sample: Some("sharpe"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: Some("rank_metrics"),
        validated_by: "cli",
        profile_key: None,
        shape: Some(
            "one of the RANK_METRICS roster, spelling-checked here; the metric itself is computed by whichever side runs",
        ),
        mode_scope: "any",
        default_kind: "far_side",
        default_value: Some("sharpe"),
        default_conditional_on: None,
        default_reason: Some(
            "The CLI forwards `None` when unwritten; `search_select`'s `resolve_rank` maps `None` to `RankChoice::Metric(RankMetric::default())`, and `RankMetric`'s `#[default]` variant is `Sharpe`. Deliberately NOT part of `Args::search_requested`, so it never routes a run to the search verb by itself.",
        ),
        short: "Which metric orders a parameter search's rows (sharpe, return, max_dd, equity, multi). A VALID value is IGNORED, not an error, on a profile with no grid.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `RANK_METRICS`, `Args::rank_by`, `Args::search_requested` and `parse_run_args`'s `\"--rank-by\"` arm; crates/vike-backtest/src/harness/search_select.rs's `resolve_rank`; crates/vike-backtest/src/harness/sweep.rs's `RankMetric`",
        refusals: &[
            Refusal {
                on: "a value outside RANK_METRICS (CLI, exit 2, before any dial or spawn)",
                message: "--rank-by must be sharpe|return|max_dd|equity|multi, got \"bogus\"",
            },
            Refusal {
                on: "typed on a profile that declares a [walkforward] table (CLI, exit 2) — that wire verb carries the profile TOML and nothing else",
                message: "--rank-by configures nothing on a walk-forward run: the wire verb it goes over carries the profile TOML and nothing else. A window's ranking is [walkforward].rank_by, in the profile, read by the one parser that runs it — and what each window searches is that same [walkforward] table's business, never a flag on this side.",
            },
            Refusal {
                on: "--rank-by multi against a daemon that does not advertise `search_method` — refused locally, nothing sent",
                message: "backtest daemon does not advertise `search_method` (advertised: {features:?}) — nothing was sent. A search METHOD (--optimizer/--euler-depth/--trials/--seed) and --rank-by multi need a newer `vike-backend backtest --addr`; this verb is capability-negotiated, not version-gated. An older daemon would DROP the field, run the exhaustive grid and report success, which is the silent downgrade this refusal exists to prevent — upgrade the daemon, or drop the flag to search the grid on the server.",
            },
            Refusal {
                on: "an invalid value reaching the engine or the server directly (not reachable from this CLI, which pre-checks)",
                message: "invalid --rank-by \"bogus\" (expected sharpe|return|max_dd|equity|multi)",
            },
        ],
    },
    FlagRow {
        long: "--script",
        value: Value::Required,
        value_name: Some("<s.rhai>"),
        sample: Some("s.rhai"),
        repeat: Repeat::LastWins,
        applies_to: &["run", "params"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: Some("strategy.params.src"),
        shape: None,
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "Writes [strategy.params].src with the file's SOURCE text; it is a client-side rewrite applied in execute, not an Override, so it does not appear in --show-effective's override list. It also TRIGGERS the implied `strategy.name = \"rhai\"` row.",
        ),
        short: "Injects the .rhai file's source into strategy.params.src (overwriting any existing src) and implies strategy.name=rhai. On the `params` sub-verb the same spelling names the script whose knobs are listed.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's inject_script_src and its SRC_KEY; crates/vike-cli/src/cmd/backtest.rs's implied_defaults",
        refusals: &[
            Refusal {
                on: "params, given together with --strategy",
                message: "--script and --strategy name two different sources for the same question — give exactly one",
            },
            Refusal {
                on: "params, unreadable file",
                message: "cannot read script {path}: {io_error}",
            },
            Refusal {
                on: "params, script that will not compile",
                message: "rhai compile error: {err}",
            },
            Refusal {
                on: "ls, show, path, tag, diff, gate, strategies",
                message: "--script does not apply to `backtest {sub}` — that flag names the strategy or script whose knobs `params` lists",
            },
        ],
    },
    FlagRow {
        long: "--seed",
        value: Value::Required,
        value_name: Some("u64"),
        sample: Some("1"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "engine",
        profile_key: None,
        shape: Some("a token, FORWARDED VERBATIM; the engine wants a u64"),
        mode_scope: "any",
        default_kind: "far_side",
        default_value: Some("0 under tpe; NO default under genetic (absence is refused)"),
        default_conditional_on: None,
        default_reason: Some(
            "CONFIRMED forwarded verbatim (same three sites). `search_select`'s `parse_seed` returns `Option<u64>` and substitutes nothing; the tpe arm applies `.unwrap_or(0)` and the genetic arm calls `require_seed`, which refuses an absent seed outright.",
        ),
        short: "Reproducibility seed for the two stochastic searchers. tpe defaults it to 0; genetic REFUSES an absent one.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `Args::seed` and `parse_run_args`'s `\"--seed\"` arm; crates/vike-backtest/src/harness/search_select.rs's `parse_seed`, `require_seed` and `resolve`",
        refusals: &[
            Refusal {
                on: "--seed under grid or euler (METHOD_KNOBS row owners are [\"tpe\", \"genetic\"], joined with \" or \")",
                message: "--seed is a tpe or genetic flag, but this run selected the \"grid\" optimizer. It was silently discarded before; it is refused now, because a knob that configures a search you did not select cannot do what it says",
            },
            Refusal { on: "a non-u64 value", message: "invalid --seed \"abc\" (expected a u64)" },
            Refusal {
                on: "--optimizer genetic with no --seed",
                message: "--optimizer genetic requires --seed <u64>. It is not defaulted, deliberately: a genetic search reports ONE sample of a distribution, and a seed nobody typed is a constant the result silently depends on — write `--seed 7` (any u64) and the run is reproducible from your own shell history",
            },
            Refusal {
                on: "written with no value, on the engine's argv path",
                message: "--seed was written with no value (expected a u64). A trailing `--seed` read as ABSENT before, which silently ran the default instead of refusing — write `--seed <value>`",
            },
        ],
    },
    FlagRow {
        long: "--set",
        value: Value::Required,
        value_name: Some("key=value"),
        sample: Some("engine.fee_rate=0.001"),
        repeat: Repeat::Accumulate,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: Some("profile_top_level_keys"),
        validated_by: "cli",
        profile_key: Some("<the dotted key typed>"),
        shape: None,
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "The universal channel — the key is whatever the operator typed, so there is nothing to default.",
        ),
        short: "Repeatable universal channel; value typed by parse_scalar. ONLY the FIRST segment is checked (against PROFILE_TOP_LEVEL_KEYS) — the rest is the far side's deny_unknown_fields, on exit rung 1.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's parse_run_args (the `\"--set\"` arm computes `let head = key.split('.').next().unwrap_or(\"\")` and tests `PROFILE_TOP_LEVEL_KEYS.contains(&head)`); crates/vike-cli/src/cmd/backtest.rs's set_profile_key",
        refusals: &[
            Refusal {
                on: "the value carries no `=` (split_once, first `=` only — the VALUE may contain further `=`)",
                message: "--set takes key=value, got {pair:?} (e.g. --set engine.fee_rate=0.001)",
            },
            Refusal {
                on: "the first dotted segment is not in PROFILE_TOP_LEVEL_KEYS — including the EMPTY head produced by a leading dot or an empty key. The trailing hole is PROFILE_TOP_LEVEL_KEYS.join(\", \").",
                message: "--set {key}: a profile has no `{head}` table — the top-level keys are {PROFILE_TOP_LEVEL_KEYS.join(\", \")}",
            },
            Refusal {
                on: "any segment is empty after the head check passed (e.g. `--set name.=1`) — raised by set_profile_key inside build_profile_toml, lifted onto the USAGE rung",
                message: "--set key {dotted_key:?} has an empty segment — a key is spelled `table.key` (or `table.sub.key`), with no leading, trailing or doubled dot",
            },
            Refusal {
                on: "the path runs THROUGH a plain value; `{}` is `segs[..=i].join(\".\")`, the blocking prefix",
                message: "`{segs[..=i].join(\".\")}` is a plain value in the profile, so `{dotted_key}` cannot nest under it",
            },
            Refusal {
                on: "the leaf names a whole table",
                message: "`{dotted_key}` names a whole table in the profile, not a single value — set its leaves instead (`{dotted_key}.<key>`)",
            },
            Refusal {
                on: "the profile root is not a TOML table",
                message: "profile root is not a TOML table",
            },
        ],
    },
    FlagRow {
        long: "--show-effective",
        value: Value::None,
        value_name: None,
        sample: None,
        repeat: Repeat::Idempotent,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: None,
        shape: None,
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some("A bare boolean; it selects an output mode rather than a value."),
        short: "Prints the resolved profile with each override's origin as TOML COMMENTS and STOPS — exit 0, no dial, no engine, no store. Rejects an inline value.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's execute (`if args.show_effective { print!(...); return Ok(()); }`); crates/vike-cli/src/cmd/backtest.rs's render_effective",
        refusals: &[Refusal {
            on: "an inline value (`--show-effective=1`)",
            message: "{flag} takes no value",
        }],
    },
    FlagRow {
        long: "--slippage",
        value: Value::Required,
        value_name: Some("RATE"),
        sample: Some("0.0005"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "engine",
        profile_key: Some("engine.slippage"),
        shape: Some("Scalar"),
        mode_scope: "any",
        default_kind: "schema",
        default_value: Some("0.0"),
        default_conditional_on: None,
        default_reason: Some(
            "`EngineCfg::slippage` is `#[serde(default)] f64` in crates/vike-backtest/src/harness/profile.rs. The CLI supplies nothing.",
        ),
        short: "Sugar for engine.slippage through parse_scalar; same typing rule as --fee and --cash.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's SUGAR (row `--slippage` -> `engine.slippage`, Shape::Scalar)",
        refusals: &[],
    },
    FlagRow {
        long: "--sort",
        value: Value::Required,
        value_name: Some("FIELD[:asc|:desc]"),
        sample: Some("sharpe:desc"),
        repeat: Repeat::LastWins,
        applies_to: &["ls"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: Some("sort_directions"),
        validated_by: "cli",
        profile_key: None,
        shape: Some("field name with optional direction suffix"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "Absent = whatever order `scan_runs`/the selector produced; no sort is applied.",
        ),
        short: "`ls` only. Direction defaults by TYPE of the surviving rows: numeric descends, text ascends; runs missing the key sort LAST in both directions. The typo check runs over the UNFILTERED scan, and an EMPTY store is excused before the fold (otherwise `all(Missing)` would be vacuously true and a fresh project would exit 2 accusing a typo).",
        evidence: "crates/vike-cli/src/cmd/runs/ls.rs's `sort_rows`",
        refusals: &[
            Refusal {
                on: "ls, with a direction suffix that is not asc/desc",
                message: "--sort direction '{other}' is not one of: asc | desc",
            },
            Refusal {
                on: "ls, when the store is non-empty and NO run carries the field",
                message: "--sort '{key}': no run carries that field. Manifest fields are run_id, kind, produced_by, started_at, finished_at, git_sha, config.path, config.name, report and detail.<path>; any other name is read as a top-level key of report.json.",
            },
            Refusal {
                on: "show, path, tag, diff, gate, params, strategies",
                message: "--sort does not apply to `backtest {sub}` — that flag narrows, orders or shapes a LISTING, and `ls` is the listing",
            },
        ],
    },
    FlagRow {
        long: "--store",
        value: Value::Required,
        value_name: Some("DIR"),
        sample: Some("/data"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "engine",
        profile_key: None,
        shape: Some("a directory path, forwarded to the spawned engine as `--store <DIR>`"),
        mode_scope: "local",
        default_kind: "far_side",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "Unwritten, no `--store` is pushed into the child argv at all and the engine resolves its own hist-store root (`crates/vike-backtest/src/backtest_cli.rs`'s `store_root`, which also consults the caller-supplied vars). This side substitutes nothing and validates nothing about the path.",
        ),
        short: "The hist-store root the LOCAL engine reads. Local-arm only: a remote run reads the SERVER's store.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `Args::store`, `parse_run_args`'s remote-arm refusal loop and `execute_local`; crates/vike-backtest/src/backtest_cli.rs's `store_root`",
        refusals: &[
            Refusal {
                on: "⚠ typed WITHOUT --local, i.e. on the remote arm (CLI, exit 2). One line, no continuations.",
                message: "--store applies to --local only — a remote run reads the SERVER's store",
            },
            Refusal { on: "written with no value", message: "--store requires a value" },
        ],
    },
    FlagRow {
        long: "--strategy",
        value: Value::Required,
        value_name: Some("NAME"),
        sample: Some("buy_hold"),
        repeat: Repeat::LastWins,
        applies_to: &["run", "params"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: Some("strategy.name"),
        shape: Some("Str"),
        mode_scope: "any",
        default_kind: "implied",
        default_value: Some("rhai"),
        default_conditional_on: Some("--script"),
        default_reason: Some(
            "implied_defaults pushes strategy.name = \"rhai\" ONLY when a script was named. With no --script the CLI supplies nothing and StrategyCfg::name carries no serde default, so the far side decides.",
        ),
        short: "Sugar for strategy.name, forwarded UNVALIDATED (the roster lives in vike-backtest and includes user strategies this crate cannot see). On the `params` sub-verb the same spelling names the roster entry whose knobs are listed.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's SUGAR (row `--strategy` -> `strategy.name`); pinned by crates/vike-cli/src/cmd/backtest.rs's a_strategy_name_is_forwarded_unvalidated; the conditional default is crates/vike-cli/src/cmd/backtest.rs's implied_defaults",
        refusals: &[
            Refusal {
                on: "params, given together with --script",
                message: "--script and --strategy name two different sources for the same question — give exactly one",
            },
            Refusal {
                on: "params, a name no roster carries",
                message: "no built-in strategy named '{name}'. The roster is: {roster joined by \", \" — PORTABLE_STRATEGIES, then SIMULATOR_ONLY names, then SCRIPT_ONLY names}",
            },
            Refusal {
                on: "ls, show, path, tag, diff, gate, strategies",
                message: "--strategy does not apply to `backtest {sub}` — that flag names the strategy or script whose knobs `params` lists",
            },
        ],
    },
    FlagRow {
        long: "--symbol",
        value: Value::Required,
        value_name: Some("SYM[,SYM]"),
        sample: Some("BTCUSDT,ETHUSDT"),
        repeat: Repeat::CommaListFold,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: Some("data.symbols"),
        shape: Some("StrList"),
        mode_scope: "any",
        default_kind: "schema",
        default_value: Some("[]"),
        default_conditional_on: None,
        default_reason: Some(
            "`DataCfg::symbols` is `#[serde(default)] Vec<String>` in crates/vike-backtest/src/harness/profile.rs; empty is the schema default and the CLI supplies nothing.",
        ),
        short: "The ONE row that does not push per occurrence: every occurrence comma-splits and accumulates, and a SINGLE data.symbols array Override is pushed after the loop, in argv order.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's parse_run_args (the Shape::StrList arm accumulates into `symbols`; the post-loop block pushes one Override with Origin::Sugar(\"--symbol\")); pinned by crates/vike-cli/src/cmd/backtest.rs's symbol_is_repeatable_and_comma_splitting_and_yields_one_override",
        refusals: &[Refusal {
            on: "any empty element after trimming — a leading, trailing or doubled comma, or an empty value. The flag name in the message is HARD-CODED `--symbol` even though the arm is generic over Shape::StrList (it is the only StrList row).",
            message: "--symbol takes SYM[,SYM…] with no empty element, got {raw:?}",
        }],
    },
    FlagRow {
        long: "--to",
        value: Value::Required,
        value_name: Some("DATE"),
        sample: Some("2026-02-01T00"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "engine",
        profile_key: Some("data.to"),
        shape: Some("Str"),
        mode_scope: "any",
        default_kind: "far_side",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "`DataCfg::to` carries NO serde default in crates/vike-backtest/src/harness/profile.rs. The CLI implies nothing.",
        ),
        short: "Sugar for data.to; same Shape::Str string-preservation rule as --from. No CLI-side date validation of any kind.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's SUGAR (row `--to` -> `data.to`, Shape::Str)",
        refusals: &[],
    },
    FlagRow {
        long: "--trades",
        value: Value::None,
        value_name: None,
        sample: None,
        repeat: Repeat::Idempotent,
        applies_to: &["show", "diff"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: None,
        shape: Some("bool"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "On `show`, absent means every section renders (`show_text`'s `all = !metrics && !trades && !config`). On `diff`, absent means no trades section.",
        ),
        short: "THE OTHER DOUBLE-OWNED FLAG. Owners: `show` (renders trades.json as a section) and `diff` (adds a third diff section over both runs' trades.json). Deliberately kept OUT of both the `show_only` and `diff_only` rosters and refused on the remaining six by its own named rule with its own sentence.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `refuse_foreign_read_flags` (the `!matches!(a.sub, ReadSub::Show | ReadSub::Diff) && a.trades` rule, and the ⚠ comment above `show_only` saying why it is not in that array); crates/vike-cli/src/cmd/runs/show.rs's `show_text`; crates/vike-cli/src/cmd/runs/diff.rs's `run_diff`",
        refusals: &[
            Refusal {
                on: "ls, path, tag, gate, params, strategies",
                message: "--trades does not apply to `backtest {sub}` — it names the stored trade ledger, which `show` renders and `diff` compares",
            },
            Refusal {
                on: "(any reading sub-verb, with a value)",
                message: "--trades takes no value",
            },
        ],
    },
    FlagRow {
        long: "--trials",
        value: Value::Required,
        value_name: Some("N"),
        sample: Some("8"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "engine",
        profile_key: None,
        shape: Some(
            "a token, FORWARDED VERBATIM — this crate parses nothing; the engine wants a positive integer",
        ),
        mode_scope: "any",
        default_kind: "far_side",
        default_value: Some("64"),
        default_conditional_on: None,
        default_reason: Some(
            "CONFIRMED forwarded verbatim: `parse_run_args` stores the raw String, `execute_local` pushes `(\"--trials\", &args.trials)` into the child argv unchanged, and `Args::wire_search` clones it into `WireSearch.trials` as a String. The default is applied only when tpe is the resolved method, by `search_select`'s `parse_trials` → `TpeConfig::DEFAULT_TRIALS` = 64.",
        ),
        short: "tpe's trial budget. Forwarded as the token you typed; the engine or the server parses and refuses it.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `Args::trials` and `parse_run_args`'s `\"--trials\"` arm; crates/vike-cli/src/cmd/backtest.rs's `execute_local`; crates/vike-backtest/src/harness/search_select.rs's `parse_trials` and `METHOD_KNOBS`; crates/vike-backtest/src/harness/tpe.rs's `DEFAULT_TRIALS`",
        refusals: &[
            Refusal {
                on: "⚠ --trials under --optimizer euler (and under grid, i.e. under no --optimizer at all) — refused from `METHOD_KNOBS` by `refuse_unowned_knob`, on BOTH routes. `{method:?}` is the Debug of the method TOKEN, so it renders quoted.",
                message: "--trials is a tpe flag, but this run selected the \"euler\" optimizer. It was silently discarded before; it is refused now, because a knob that configures a search you did not select cannot do what it says",
            },
            Refusal {
                on: "--trials with no --optimizer written at all (name falls back to DEFAULT_SEARCH_METHOD)",
                message: "--trials is a tpe flag, but this run selected the \"grid\" optimizer. It was silently discarded before; it is refused now, because a knob that configures a search you did not select cannot do what it says",
            },
            Refusal {
                on: "a value that is not a positive integer, under tpe (engine/server)",
                message: "invalid --trials \"abc\" (expected a positive integer)",
            },
            Refusal {
                on: "written with no value, on the engine's argv path (`required_value`)",
                message: "--trials was written with no value (expected a positive integer). A trailing `--trials` read as ABSENT before, which silently ran the default instead of refusing — write `--trials <value>`",
            },
            Refusal {
                on: "against a daemon that does not advertise `search_method` — refused locally, nothing sent",
                message: "backtest daemon does not advertise `search_method` (advertised: {features:?}) — nothing was sent. A search METHOD (--optimizer/--euler-depth/--trials/--seed) and --rank-by multi need a newer `vike-backend backtest --addr`; this verb is capability-negotiated, not version-gated. An older daemon would DROP the field, run the exhaustive grid and report success, which is the silent downgrade this refusal exists to prevent — upgrade the daemon, or drop the flag to search the grid on the server.",
            },
        ],
    },
    FlagRow {
        long: "--venue",
        value: Value::Required,
        value_name: Some("NAME"),
        sample: Some("binance"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: Some("data.venue"),
        shape: Some("Str"),
        mode_scope: "any",
        default_kind: "schema",
        default_value: Some("None"),
        default_conditional_on: None,
        default_reason: Some(
            "`DataCfg::venue` is `#[serde(default)] Option<String>` in crates/vike-backtest/src/harness/profile.rs, so an absent venue is None (the cross-venue [[data.series]] form is the alternative). The CLI supplies nothing.",
        ),
        short: "Sugar for data.venue, value trimmed and written as a TOML string. No roster check here — a venue name is forwarded unvalidated.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's SUGAR (row `--venue` -> `data.venue`, Shape::Str); crates/vike-backtest/src/harness/profile.rs's DataCfg",
        refusals: &[Refusal {
            on: "a missing or flag-shaped value",
            message: "{flag} requires a value",
        }],
    },
    FlagRow {
        long: "--where",
        value: Value::Required,
        value_name: Some("EXPR"),
        sample: Some("kind=backtest,sharpe>1"),
        repeat: Repeat::LastWins,
        applies_to: &["ls"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: Some("ls_field_vocabulary"),
        validated_by: "cli",
        profile_key: None,
        shape: Some("predicate expression"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some("Absent = no filtering."),
        short: "`ls` only. Grammar: EXPR := TERM (\",\" TERM)* ; TERM := FIELD OP VALUE ; OP := >= | <= | != | ~ | = | > | < (two-character operators tried FIRST). Comma is AND; there is no OR and no grouping. FIELD resolves through the same `ls::field_of` vocabulary `--sort`/`--cols` use.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `refuse_foreign_read_flags` (`ls_only`); crates/vike-cli/src/cmd/runs/where_expr.rs's `OPS` and `filter`",
        refusals: &[
            Refusal {
                on: "the expression contains a '|' anywhere ({expr} = the whole raw expression, untrimmed)",
                message: "--where '{expr}': this grammar has no OR — terms are joined by a comma, which means AND (`kind=backtest,sharpe>1`). Run `ls` twice for a disjunction.",
            },
            Refusal {
                on: "a term between two commas is empty ({expr} = the whole raw expression; OP_HELP is the literal '>= | <= | != | ~ | = | > | <')",
                message: "--where '{expr}': an empty term — terms are joined by a single comma (FIELD OP VALUE, with OP one of: >= | <= | != | ~ | = | > | <)",
            },
            Refusal {
                on: "a term contains none of the seven operators ({term} = the TRIMMED term, not the whole expression)",
                message: "--where '{term}': no operator — a term is FIELD OP VALUE, with OP one of: >= | <= | != | ~ | = | > | <",
            },
            Refusal {
                on: "the text before the operator is empty, e.g. '>1' ({term} = the trimmed term, {op} = the matched operator)",
                message: "--where '{term}': no field before '{op}' — a term is FIELD OP VALUE, with OP one of: >= | <= | != | ~ | = | > | <",
            },
        ],
    },
    FlagRow {
        long: "--write-profile",
        value: Value::Required,
        value_name: Some("<out.toml>"),
        sample: Some("out.toml"),
        repeat: Repeat::LastWins,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: None,
        shape: None,
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some("Writes a FILE, not a profile key."),
        short: "Writes the fully-resolved profile (after --preset and --script rewrites) and then RUNS. Refuses an existing path outright. Repeating it silently LAST-WINS.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's execute (the `args.write_profile` block sits BELOW both client-side rewrites and above the show_effective return)",
        refusals: &[
            Refusal {
                on: "the path already exists (USAGE rung, exit 2)",
                message: "--write-profile {path} already exists, and this command will not overwrite a profile — delete it, or name a different path",
            },
            Refusal {
                on: "the write fails (exit rung 1)",
                message: "cannot write the profile to {path}: {e}",
            },
        ],
    },
];

/// Every value roster a flag is checked against.
pub const ROSTERS: [RosterRow; 24] = [
    RosterRow {
        id: "all_subcommands",
        members: &["run", "ls", "show", "path", "tag", "diff", "gate", "params", "strategies"],
        refused_members: &[],
        match_rule: "exact",
        derived_from: Some("crates/vike-cli/src/cmd/backtest.rs's SUBCOMMANDS"),
        admission: None,
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `SUBCOMMANDS` and `subcommand_roster` (which renders it joined by \" | \" into every roster-naming refusal)",
    },
    RosterRow {
        id: "data_kinds",
        members: &["bar", "tick"],
        refused_members: &[],
        match_rule: "exact",
        derived_from: None,
        admission: Some(
            "HAND-WRITTEN COPY of the two spellings `DataKind` accepts, for the same reason as the roster above — this crate links no engine crate. Checked through the shared one_of helper, which is a SPELLING check and never a second implementation of what the value MEANS. The comparison is `roster.contains(&value)` on the already-trimmed string, so it is case-SENSITIVE.",
        ),
        evidence: "crates/vike-cli/src/cmd/backtest.rs's DATA_KINDS, consumed by crates/vike-cli/src/cmd/backtest.rs's one_of",
    },
    RosterRow {
        id: "exit_ladder",
        members: &[
            "0 Ok",
            "1 Failed",
            "2 Usage",
            "3 Connect",
            "4 Refused",
            "5 Venue",
            "6 Breach",
            "7 Empty",
        ],
        refused_members: &[],
        match_rule: "exact",
        derived_from: Some(
            "crates/vike-cli/src/exit.rs's `Exit` (`#[repr(u8)]`; the discriminants ARE the exit codes, converted by `impl From<Exit> for ExitCode`)",
        ),
        admission: None,
        evidence: "crates/vike-cli/src/exit.rs's `Exit`, pinned as literals by its own `the_rungs_are_the_numbers_scripts_branch_on`.",
    },
    RosterRow {
        id: "failif_metric_aliases",
        members: &["max_dd -> max_drawdown", "return -> total_return", "equity -> final_equity"],
        refused_members: &[],
        match_rule: "exact",
        derived_from: Some("crates/vike-cli/src/cmd/runs/failif.rs's METRIC_ALIASES"),
        admission: None,
        evidence: "const METRIC_ALIASES: &[(&str, &str)] = &[(\"max_dd\", \"max_drawdown\"), (\"return\", \"total_return\"), (\"equity\", \"final_equity\")]; resolved in parse_one by `.find(|(alias, _)| *alias == metric)` — a case-SENSITIVE, whole-string comparison, with map_or_else falling back to the metric as typed.",
    },
    RosterRow {
        id: "failif_metrics",
        members: &[],
        refused_members: &[],
        match_rule: "open_tail",
        derived_from: None,
        admission: Some(
            "OPEN — there is NO metric roster. parse_one resolves a metric against METRIC_ALIASES and otherwise keeps the spelling verbatim as the JSON key, so ANY key of report.json (and any key it gains later) is accepted. An unknown metric is NOT a parse error.",
        ),
        evidence: "crates/vike-cli/src/cmd/runs/failif.rs's parse_one (`.map_or_else(|| metric.to_string(), ...)`) and the module doc's fourth rule, 'A metric is resolved against the DOCUMENT, not a roster'; crates/vike-cli/src/cmd/runs/failif.rs's judge returns Outcome::Unevaluated when either side carries no finite number under the key.",
    },
    RosterRow {
        id: "ls_default_cols",
        members: &["run_id", "kind", "started_at", "report"],
        refused_members: &[],
        match_rule: "exact",
        derived_from: Some("crates/vike-cli/src/cmd/runs/ls.rs's DEFAULT_COLS"),
        admission: None,
        evidence: "crates/vike-cli/src/cmd/runs/ls.rs's `DEFAULT_COLS`, used by `run_ls` when `--cols` is absent",
    },
    RosterRow {
        id: "ls_field_vocabulary",
        members: &[
            "run_id",
            "kind",
            "produced_by",
            "started_at",
            "finished_at",
            "git_sha",
            "config.path",
            "config.name",
            "report",
            "detail.<dotted.path>",
            "<any other name> = a TOP-LEVEL key of report.json",
        ],
        refused_members: &[],
        match_rule: "open_tail",
        derived_from: Some("crates/vike-cli/src/cmd/runs/ls.rs's field_of"),
        admission: None,
        evidence: "crates/vike-cli/src/cmd/runs/ls.rs's `field_of` — the ONE resolver `--cols`, `--sort` and `--where` all read through. The last two rows are FALLTHROUGHS, not enumerated names, so this roster has no closed membership.",
    },
    RosterRow {
        id: "method_knobs",
        members: &["--euler-depth => euler", "--trials => tpe", "--seed => tpe, genetic"],
        refused_members: &[],
        match_rule: "ascii_ci",
        derived_from: Some("vike_backtest::harness::search_select::METHOD_KNOBS"),
        admission: None,
        evidence: "crates/vike-backtest/src/harness/search_select.rs's `METHOD_KNOBS` (`pub const METHOD_KNOBS: &[(&str, &[&str])]`), `SearchSelection::knob`, `resolve` and `refuse_unowned_knob`; crates/vike-backtest/src/backtest_cli.rs's `parse_search_flags`, which iterates the SAME table on argv PRESENCE before any value is parsed",
    },
    RosterRow {
        id: "optimizers",
        members: &["grid", "euler", "tpe", "genetic"],
        refused_members: &[],
        match_rule: "exact",
        derived_from: Some("vike_datahub_client::SEARCH_METHODS"),
        admission: None,
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `OPTIMIZERS` (declared `const OPTIMIZERS: [&str; 4] = vike_datahub_client::SEARCH_METHODS;`) and `one_of`; crates/vike-datahub-client/src/proto.rs's `SEARCH_METHODS`; crates/vike-cli/tests/backtest_cli.rs's `every_roster_method_is_accepted_on_both_routes`, which iterates the protocol const over both routes",
    },
    RosterRow {
        id: "params_refused",
        members: &[
            "--profile",
            "--preset",
            "--local",
            "--set",
            "--store",
            "--engine",
            "--rank-by",
            "--optimizer",
            "--euler-depth",
            "--trials",
            "--seed",
            "--venue",
            "--symbol",
            "--interval",
            "--from",
            "--to",
            "--kind",
            "--cash",
            "--fee",
            "--slippage",
            "--param",
            "--write-profile",
            "--show-effective",
        ],
        refused_members: &[
            "--profile :: --profile does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--preset :: --preset does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--local :: --local does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--set :: --set does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--store :: --store does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--engine :: --engine does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--rank-by :: --rank-by does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--optimizer :: --optimizer does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--euler-depth :: --euler-depth does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--trials :: --trials does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--seed :: --seed does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--venue :: --venue does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--symbol :: --symbol does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--interval :: --interval does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--from :: --from does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--to :: --to does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--kind :: --kind does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--cash :: --cash does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--fee :: --fee does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--slippage :: --slippage does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--param :: --param does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--write-profile :: --write-profile does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
            "--show-effective :: --show-effective does not apply to `params`, which reads the script or the strategy roster on this machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli backtest run`",
        ],
        match_rule: "exact",
        derived_from: Some("crates/vike-cli/src/cmd/backtest.rs's PARAMS_REFUSED"),
        admission: None,
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `PARAMS_REFUSED`, consumed by `parse_read`'s `other if a.sub == ReadSub::Params && PARAMS_REFUSED.contains(&other)` guard",
    },
    RosterRow {
        id: "params_strategy_roster",
        members: &[
            "buy_hold",
            "grid",
            "dca_accumulate",
            "spread_maker",
            "gueant_maker",
            "trailing_scalper",
            "momentum",
            "funding_carry",
            "funding_capture",
            "pairs_zscore",
            "rotation_top_k",
            "bracket_per_symbol",
            "gated_weights",
            "caps_sizers_mask",
            "tick_pair_mse",
            "cheap_catch_updown_fair_value",
            "sport_copy_follower",
            "rhai",
        ],
        refused_members: &[],
        match_rule: "exact",
        derived_from: Some(
            "crates/vike-cli/src/cmd/params.rs's roster — vike_strategy::PORTABLE_STRATEGIES, then SIMULATOR_ONLY names, then SCRIPT_ONLY names, in that order",
        ),
        admission: None,
        evidence: "crates/vike-cli/src/cmd/params.rs's `roster` and `describe`; crates/vike-strategy/src/registry.rs's `PORTABLE_STRATEGIES`, `SIMULATOR_ONLY` and `SCRIPT_ONLY`. Order matters — the refusal prints this roster joined by \", \".",
    },
    RosterRow {
        id: "path_file_sugar",
        members: &["manifest", "manifest.json", "report", "report.json"],
        refused_members: &[],
        match_rule: "open_tail",
        derived_from: Some("crates/vike-cli/src/cmd/runs/path.rs's resolve_file"),
        admission: None,
        evidence: "crates/vike-cli/src/cmd/runs/path.rs's `resolve_file` — `manifest`/`manifest.json` → MANIFEST_FILE (\"manifest.json\"), `report`/`report.json` → REPORT_FILE (\"report.json\"); anything else joins VERBATIM, so this is sugar over an open set",
    },
    RosterRow {
        id: "profile_top_level_keys",
        members: &[
            "name",
            "data",
            "engine",
            "strategy",
            "risk",
            "paramscan",
            "sweep",
            "walkforward",
        ],
        refused_members: &[],
        match_rule: "exact",
        derived_from: None,
        admission: Some(
            "HAND-WRITTEN COPY, deliberately. crates/vike-cli/Cargo.toml states this crate links no vike-backtest and no engine crate, so `BacktestProfile` is not nameable here at all and nothing can derive it. It is the FIRST-SEGMENT roster only — a full key roster would be ~90 engine.* names against a schema that grew sixteen fields in one PR. `base_dir` is deliberately absent (it is `#[serde(skip)]`, not a TOML key). BOTH `paramscan` and `sweep` are members: owner ruling R2 renamed the section and the loader keeps `sweep` as a PERMANENT serde alias, so an omitted alias row would make `--set sweep.fast=…` a CLI usage error against a key the engine accepts. Gated from the test tree (where the dev-dependency reaches) by crates/vike-cli/tests/backtest_flags_schema.rs's every_cli_top_level_key_is_one_the_profile_declares.",
        ),
        evidence: "crates/vike-cli/src/cmd/backtest.rs's PROFILE_TOP_LEVEL_KEYS (declared `[&str; 8]`)",
    },
    RosterRow {
        id: "rank_metrics",
        members: &["sharpe", "return", "max_dd", "equity", "multi"],
        refused_members: &[
            "multi :: ⚠ NOT refused by today's engine-side `--rank-by` parser. Both routes resolve through crates/vike-backtest/src/harness/search_select.rs's `resolve_rank`, whose FIRST arm is `Some(s) if s.eq_ignore_ascii_case(\"multi\")` — the engine binary reaches it via `parse_search_flags` and the compute server via `run_paramscan_profile`, so `--rank-by multi` is accepted on `--local` and on `--addr` alike (crates/vike-cli/tests/search_walkforward_cli.rs's `every_search_knob_now_reaches_the_dial` drives `[\"--rank-by\", \"multi\"]` to a CONNECT failure, exit 3, proving it was not a usage error). It IS refused by the FOUR-ARM parser crates/vike-backtest/src/harness/sweep.rs's `RankMetric::from_str_ci`, which returns None for it — and that parser is what a daemon predating the `search_method` capability uses, and what crates/vike-backtest/src/harness/profile.rs's `WalkforwardCfg::rank_metric` uses for the PROFILE key `[walkforward].rank_by`. Verbatim from that profile-key path: `unknown walkforward.rank_by \"multi\" (want sharpe | return | max_dd | equity; absent = sharpe)`. Against an old daemon the CLI never gets that far — crates/vike-datahub-client/src/client.rs's `run_paramscan_profile` pre-refuses `rank_by == \"multi\"` locally under FEATURE_SEARCH_METHOD, without sending.",
        ],
        match_rule: "exact",
        derived_from: None,
        admission: Some(
            "HAND-WRITTEN. `const RANK_METRICS: [&str; 5] = [\"sharpe\", \"return\", \"max_dd\", \"equity\", \"multi\"];` is a literal in crates/vike-cli/src/cmd/backtest.rs. Nothing derives it: `vike_backtest::harness::RankMetric` is not reachable from this crate (vike-cli links no engine crate, deliberately), the protocol crate exports no rank roster, and — unlike OPTIMIZERS — NO test iterates it against the engine or the server. Its first four entries are the CLI names `RankMetric::from_str_ci` accepts; the fifth, `multi`, is the composite objective `search_select::resolve_rank` special-cases above that parser.",
        ),
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `RANK_METRICS`; crates/vike-backtest/src/harness/search_select.rs's `resolve_rank`; crates/vike-backtest/src/harness/sweep.rs's `RankMetric::from_str_ci`; crates/vike-backtest/src/harness/profile.rs's `WalkforwardCfg::rank_metric`; crates/vike-datahub-client/src/client.rs's `run_paramscan_profile`",
    },
    RosterRow {
        id: "read_subcommands",
        members: &["ls", "show", "path", "tag", "diff", "gate", "params", "strategies"],
        refused_members: &[],
        match_rule: "exact",
        derived_from: Some("crates/vike-cli/src/cmd/backtest.rs's READ_SUBCOMMANDS"),
        admission: None,
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `READ_SUBCOMMANDS` and `ReadSub::as_str`; `ReadSub::from_token` resolves a typed token by exact string equality against `as_str`",
    },
    RosterRow {
        id: "retired_top_level_commands",
        members: &["sweep", "walkforward", "study"],
        refused_members: &[],
        match_rule: "exact",
        derived_from: Some("crates/vike-cli/src/lib.rs's `RETIRED_COMMANDS`"),
        admission: None,
        evidence: "crates/vike-cli/src/lib.rs's `RETIRED_COMMANDS`, matched in `dispatch` BEFORE the unknown-command catch-all; the message goes to stderr as `vike-cli: {why}` and the process exits on `Exit::Usage` (2). Deliberately NOT in `COMMANDS` — help must not advertise a verb that refuses.",
    },
    RosterRow {
        id: "selector_doc_table_forms",
        members: &[
            "<full run id>, e.g. 1789213143-8821-01",
            "<unique short prefix>, e.g. 1789213",
            "@last",
            "@last:<kind>, e.g. @last:search",
            "@<mark>, e.g. @baseline/momentum",
            "<id>#N, e.g. <id>#12",
        ],
        refused_members: &[
            "<id>#N :: '{sel}': the `<id>#N` child form is not available yet — it names trial N of a search or window N of a walk-forward, and neither writes a child run today (a search returns before a run id is minted at all). It ships with the search-persistence stage. For now name the parent outright: '{head}'.",
        ],
        match_rule: "exact",
        derived_from: Some("crates/vike-cli/src/cmd/runs/selector.rs's module `//!` table"),
        admission: None,
        evidence: "The `//!` table at the head of crates/vike-cli/src/cmd/runs/selector.rs. It carries ONE row FORMS does not: the `<id>#N` child form, documented deliberately because it is RECOGNISED and refused rather than silently missing. crates/vike-cli/src/cmd/runs/selector.rs's the_doc_table_and_the_refusal_agree pins both directions.",
    },
    RosterRow {
        id: "selector_forms",
        members: &["<run-id>", "<unique-prefix>", "@last", "@last:<kind>", "@<mark>"],
        refused_members: &[],
        match_rule: "exact",
        derived_from: Some("crates/vike-cli/src/cmd/runs/selector.rs's FORMS"),
        admission: None,
        evidence: "crates/vike-cli/src/cmd/runs/selector.rs's `FORMS`. ⚠ crates/vike-cli/src/cmd/backtest.rs's `required_selector` prints only FOUR of these five — it omits `@<mark>`.",
    },
    RosterRow {
        id: "selector_refusal_forms",
        members: &["<run-id>", "<unique-prefix>", "@last", "@last:<kind>"],
        refused_members: &[],
        match_rule: "exact",
        derived_from: None,
        admission: Some(
            "HAND-WRITTEN, and it DISAGREES with FORMS. This four-name list is a literal inside a format! template in another crate-module; nothing derives it from FORMS and no test compares the two.",
        ),
        evidence: "crates/vike-cli/src/cmd/backtest.rs's required_selector — the literal `\"`backtest {}` requires a run selector — one of: <run-id> | <unique-prefix> | @last | @last:<kind>\"`. VERIFIED against crates/vike-cli/src/cmd/runs/selector.rs's FORMS, which names five: the brief's claim is correct and `@<mark>` is exactly the one missing. This refusal fires on `backtest show|tag|gate|path|diff` with no positional (or a whitespace-only one), so an operator who reaches it is told four of the five forms their selector could take.",
    },
    RosterRow {
        id: "show_export_values",
        members: &["trades", "equity", "fills"],
        refused_members: &[
            "fills :: --export fills is not available — fills are not stored at ANY size. A backtest computes them, consumes them into report.json's scalars and drops them; only the trade LEDGER and the equity curve survive. `--export trades` and `--export equity` both work today.",
        ],
        match_rule: "exact",
        derived_from: Some(
            "crates/vike-cli/src/cmd/runs/show.rs's EXPORTABLE (the served half) plus export_document's `fills` arm",
        ),
        admission: None,
        evidence: "crates/vike-cli/src/cmd/runs/show.rs's `EXPORTABLE` and `export_document` (the value is `spec.trim()`ed, then matched by exact string)",
    },
    RosterRow {
        id: "sort_directions",
        members: &["asc", "desc"],
        refused_members: &[],
        match_rule: "exact",
        derived_from: Some("crates/vike-cli/src/cmd/runs/ls.rs's sort_rows"),
        admission: None,
        evidence: "crates/vike-cli/src/cmd/runs/ls.rs's `sort_rows` (the `spec.split_once(':')` match, which accepts only the literals \"asc\" and \"desc\")",
    },
    RosterRow {
        id: "top_level_commands",
        members: &[
            "backtest",
            "data",
            "config",
            "mcp",
            "trade",
            "report",
            "research",
            "secrets",
            "backend",
            "datahub",
            "init",
            "indicators",
        ],
        refused_members: &[],
        match_rule: "exact",
        derived_from: Some(
            "crates/vike-cli/src/lib.rs's `COMMANDS` (name, one-line summary) — the source for both `print_help` and the unknown-command hint, and the array the skills generator renders every `skills/*/SKILL.md` verb table from",
        ),
        admission: None,
        evidence: "crates/vike-cli/src/lib.rs's `COMMANDS`; each name has a matching arm in `dispatch`.",
    },
    RosterRow {
        id: "unbuilt_renderers",
        members: &["--html", "--breakdown", "--attribution", "--drawdowns"],
        refused_members: &[
            "--drawdowns :: --drawdowns is not available yet — it needs a drawdown-table renderer, which nothing in this tree has built. The EQUITY CURVE it would read is in the run record now (series.json), so this is the renderer that is missing rather than the data — `--export equity` hands you the curve today. What `show` can render today: --metrics, --trades, --config, --export trades|equity, --json, --out.",
            "--breakdown :: --breakdown is not available yet — it needs the PER-BAR RETURNS. The stored equity curve is DECIMATED once a run exceeds the sample cap (series.json's `stride` above 1 means samples were dropped), so a period breakdown computed from it would disagree with report.json's own numbers. What `show` can render today: --metrics, --trades, --config, --export trades|equity, --json, --out.",
            "--attribution :: --attribution is not available yet — it needs a per-source attribution the engine does not compute at all. What `show` can render today: --metrics, --trades, --config, --export trades|equity, --json, --out.",
            "--html :: --html is not available yet — it needs a tearsheet renderer, which nothing in this tree has built. What `show` can render today: --metrics, --trades, --config, --export trades|equity, --json, --out.",
        ],
        match_rule: "exact",
        derived_from: Some("crates/vike-cli/src/cmd/runs/show.rs's UNBUILT_RENDERERS"),
        admission: None,
        evidence: "crates/vike-cli/src/cmd/runs/show.rs's `UNBUILT_RENDERERS` and `refuse_an_unbuilt_renderer`; matched in crates/vike-cli/src/cmd/backtest.rs's `parse_read` by a guard placed AFTER every literal flag arm and applying to EVERY reading sub-verb, not just `show`",
    },
    RosterRow {
        id: "where_operators",
        members: &[">=", "<=", "!=", "~", "=", ">", "<"],
        refused_members: &[],
        match_rule: "exact",
        derived_from: Some("crates/vike-cli/src/cmd/runs/where_expr.rs's OPS"),
        admission: None,
        evidence: "crates/vike-cli/src/cmd/runs/where_expr.rs's `OPS` (declared LONGEST FIRST — the order is the parse) and `OP_HELP`",
    },
];

/// The published assets, by file name.
///
/// PURE: opens no file, reads no clock, dials nothing. That is what lets the gate call it directly
/// instead of spawning the binary — and an in-process gate has no skip path.
pub fn rendered_files() -> BTreeMap<&'static str, String> {
    let mut out = BTreeMap::new();
    out.insert(CLI_JSON, render_cli_json());
    out
}

/// Render `cli.json`.
///
/// Ends with a newline, like every other text asset this repo publishes.
fn render_cli_json() -> String {
    let flags: Vec<serde_json::Value> = FLAGS.iter().map(flag_json).collect();
    let rosters: Vec<serde_json::Value> = ROSTERS.iter().map(roster_json).collect();
    let doc = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "plane": PLANE,
        "sub_verbs": SUB_VERBS,
        "sub_verb_required": true,
        "grammar": {
            "value_forms": ["--flag value", "--flag=value"],
            "split_on_first_equals_only": true,
            "help_stream": "stdout",
            "help_exit": 0,
        },
        "flags": flags,
        "rosters": rosters,
    });
    let mut s = serde_json::to_string_pretty(&doc).expect("the surface document is plain data");
    s.push('\n');
    s
}

fn flag_json(f: &FlagRow) -> serde_json::Value {
    serde_json::json!({
        "long": f.long,
        "value": match f.value {
            Value::None => "none",
            Value::Required => "required",
        },
        "value_name": f.value_name,
        "sample": f.sample,
        "repeat": match f.repeat {
            Repeat::LastWins => "last_wins",
            Repeat::Accumulate => "accumulate",
            Repeat::CommaListFold => "comma_list_fold",
            Repeat::Idempotent => "idempotent",
        },
        "applies_to": f.applies_to,
        "refusal_reaches": f.refusal_reaches,
        "status": match f.status {
            Status::Ships => "ships",
            Status::Unbuilt => "unbuilt",
            Status::Retired => "retired",
        },
        "roster_id": f.roster_id,
        "validated_by": f.validated_by,
        "profile_key": f.profile_key,
        "shape": f.shape,
        "mode_scope": f.mode_scope,
        "default": {
            "kind": f.default_kind,
            "value": f.default_value,
            "conditional_on": f.default_conditional_on,
            "reason": f.default_reason,
        },
        "short": f.short,
        "evidence": f.evidence,
        "refusals": f
            .refusals
            .iter()
            .map(|r| serde_json::json!({ "on": r.on, "message": r.message }))
            .collect::<Vec<_>>(),
    })
}

fn roster_json(r: &RosterRow) -> serde_json::Value {
    serde_json::json!({
        "id": r.id,
        "members": r.members,
        "refused_members": r.refused_members,
        "match": r.match_rule,
        "derived_from": r.derived_from,
        "admission": r.admission,
        "evidence": r.evidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every flag name is unique. A duplicate renders twice and the second silently wins.
    #[test]
    fn flag_names_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for f in FLAGS.iter() {
            assert!(seen.insert(f.long), "{} appears twice in FLAGS", f.long);
        }
    }

    /// Every flag name is spelled as a long flag.
    #[test]
    fn flag_names_are_long_flags() {
        for f in FLAGS.iter() {
            assert!(f.long.starts_with("--"), "{} is not spelled as a long flag", f.long);
        }
    }

    /// ⚠ A flag that takes a value must carry a SAMPLE, and a bare switch must not.
    ///
    /// The sample is what retires the two hand-written arity tables this export exists to replace,
    /// so a row with no sample cannot drive them and the retirement stalls on exactly that row.
    #[test]
    fn arity_and_sample_agree() {
        for f in FLAGS.iter() {
            match f.value {
                Value::Required => {
                    let ok = match f.sample {
                        Some(s) => !s.is_empty(),
                        None => false,
                    };
                    assert!(ok, "{} takes a value and carries no sample", f.long);
                }
                Value::None => {
                    let ok = match f.sample {
                        Some(s) => s.is_empty(),
                        None => true,
                    };
                    assert!(
                        ok,
                        "{} is a bare switch and carries the sample {:?}",
                        f.long, f.sample
                    );
                }
            }
        }
    }

    /// A bare switch cannot be [`Repeat::LastWins`]: there is no earlier value to overwrite.
    #[test]
    fn a_bare_switch_is_idempotent() {
        for f in FLAGS.iter() {
            if f.value == Value::None {
                assert_eq!(
                    f.repeat,
                    Repeat::Idempotent,
                    "{} is a bare switch, so its repeat cannot be {:?}",
                    f.long,
                    f.repeat
                );
            }
        }
    }

    /// ⚠ **An unbuilt flag accepts NOWHERE**, and must say where its refusal reaches instead.
    ///
    /// Written as a test because the gathered data had these the other way round: the four unbuilt
    /// renderers listed all eight reading sub-verbs in `applies_to`, which records where the REFUSAL
    /// reaches. A consumer reading that field as "accepts" would have published four flags that
    /// accept nothing as accepted everywhere.
    #[test]
    fn an_unbuilt_flag_accepts_nowhere_and_says_where_its_refusal_reaches() {
        for f in FLAGS.iter() {
            if f.status == Status::Unbuilt {
                assert!(
                    f.applies_to.is_empty(),
                    "unbuilt {} claims to be accepted on {:?}",
                    f.long,
                    f.applies_to
                );
                assert!(
                    !f.refusal_reaches.is_empty(),
                    "unbuilt {} says nowhere its refusal reaches",
                    f.long
                );
            }
        }
    }

    /// A flag that ships is accepted somewhere.
    #[test]
    fn a_shipping_flag_is_accepted_somewhere() {
        for f in FLAGS.iter() {
            if f.status == Status::Ships {
                assert!(!f.applies_to.is_empty(), "{} ships and is accepted nowhere", f.long);
            }
        }
    }

    /// Every sub-verb a row names is a real one.
    #[test]
    fn every_named_sub_verb_is_real() {
        for f in FLAGS.iter() {
            for v in f.applies_to.iter().chain(f.refusal_reaches.iter()) {
                assert!(
                    SUB_VERBS.contains(v),
                    "{} names the sub-verb {:?}, which does not exist",
                    f.long,
                    v
                );
            }
        }
    }

    /// Every `roster_id` a flag names resolves to a declared roster.
    #[test]
    fn every_named_roster_resolves() {
        for f in FLAGS.iter() {
            if let Some(id) = f.roster_id {
                assert!(
                    ROSTERS.iter().any(|r| r.id == id),
                    "{} names the roster {:?}, which is not in ROSTERS",
                    f.long,
                    id
                );
            }
        }
    }

    /// Roster ids are unique.
    #[test]
    fn roster_ids_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for r in ROSTERS.iter() {
            assert!(seen.insert(r.id), "roster {} is declared twice", r.id);
        }
    }

    /// ⚠ A roster nothing derives must ADMIT that in writing, so an ungated hand copy is visible
    /// rather than assumed away.
    #[test]
    fn an_underived_roster_admits_it() {
        for r in ROSTERS.iter() {
            if r.derived_from.is_none() {
                let ok = match r.admission {
                    Some(a) => !a.is_empty(),
                    None => false,
                };
                assert!(ok, "roster {} is derived from nothing and admits nothing", r.id);
            }
        }
    }

    /// ⚠ The match rule must be one this export defines — `open_tail` above all, because a roster
    /// carrying it may not be rendered as an exhaustive list.
    #[test]
    fn match_rules_are_known() {
        for r in ROSTERS.iter() {
            assert!(
                matches!(r.match_rule, "exact" | "ascii_ci" | "open_tail"),
                "roster {} carries the unknown match rule {:?}",
                r.id,
                r.match_rule
            );
        }
    }

    /// `validated_by` is one of the three values a renderer knows.
    #[test]
    fn validated_by_is_known() {
        for f in FLAGS.iter() {
            assert!(
                matches!(f.validated_by, "cli" | "engine" | "none"),
                "{} carries the unknown validated_by {:?}",
                f.long,
                f.validated_by
            );
        }
    }

    /// `default.kind` is one of the four precedence classes, `far_side` included.
    #[test]
    fn default_kinds_are_known() {
        for f in FLAGS.iter() {
            assert!(
                matches!(f.default_kind, "schema" | "implied" | "far_side" | "none"),
                "{} carries the unknown default kind {:?}",
                f.long,
                f.default_kind
            );
        }
    }

    /// ⚠ A CONDITIONAL default must name the flag it is conditional on, and that flag must exist.
    ///
    /// Stated because the flat rendering is the dangerous one: `--strategy`'s implied `rhai` applies
    /// only under `--script`, and a table printing it unconditionally publishes a default the CLI
    /// supplies half the time.
    #[test]
    fn a_conditional_default_names_a_real_flag() {
        for f in FLAGS.iter() {
            if let Some(on) = f.default_conditional_on {
                assert!(
                    FLAGS.iter().any(|o| o.long == on),
                    "{}'s default is conditional on {:?}, which is not a flag",
                    f.long,
                    on
                );
            }
        }
    }

    /// Every row says where it was read from.
    #[test]
    fn every_row_carries_evidence() {
        for f in FLAGS.iter() {
            assert!(!f.evidence.is_empty(), "{} carries no evidence", f.long);
        }
        for r in ROSTERS.iter() {
            assert!(!r.evidence.is_empty(), "roster {} carries no evidence", r.id);
        }
    }

    /// ⚠ **Citations are by SYMBOL, never by line.**
    ///
    /// A `path:NNN` rots silently — which is why this tree's citation gate rejects one outright —
    /// and this table would carry that rot into a PUBLISHED asset, where nothing in this repo scans
    /// it.
    #[test]
    fn no_evidence_cites_a_line_number() {
        fn cites_a_line(s: &str) -> bool {
            s.split_whitespace().any(|w| {
                let Some((head, tail)) = w.rsplit_once(':') else { return false };
                head.ends_with(".rs")
                    && !tail.is_empty()
                    && tail.chars().all(|c| c.is_ascii_digit())
            })
        }
        for f in FLAGS.iter() {
            assert!(!cites_a_line(f.evidence), "{} cites a line number: {}", f.long, f.evidence);
        }
        for r in ROSTERS.iter() {
            assert!(
                !cites_a_line(r.evidence),
                "roster {} cites a line number: {}",
                r.id,
                r.evidence
            );
        }
    }

    /// A refusal carries a message.
    #[test]
    fn every_refusal_carries_a_message() {
        for f in FLAGS.iter() {
            for r in f.refusals.iter() {
                assert!(
                    !r.message.is_empty(),
                    "{} carries an empty refusal for {:?}",
                    f.long,
                    r.on
                );
                assert!(!r.on.is_empty(), "{} carries a refusal that names no subject", f.long);
            }
        }
    }

    /// ⚠ **No row may cite a path the public mirror withholds.**
    ///
    /// This table is PUBLISHED — it ships as a release asset and the documentation site renders its
    /// prose onto a public page. `scripts/`, `docs/`, `.github/`, `content/`, the `justfile` and
    /// every `CLAUDE.md` are excluded from the mirror by `scripts/publish_mirror.sh`'s `DENY`, so a
    /// citation naming one of them renders as a link into nothing for every reader outside this
    /// repository.
    ///
    /// Found by reading the fixture against the docs site's own `citations.test.mjs`, which treats
    /// exactly these prefixes as withheld and fails its build on one — so without this test the
    /// first symptom would have been a RED BUILD IN ANOTHER REPOSITORY, caused by a string in this
    /// one. `rosters[top_level_commands].derived_from` cited `scripts/gen_skills.sh` and was the
    /// single instance.
    ///
    /// ⚠ `crates/` paths are fine and deliberately not matched: the mirror publishes the source
    /// tree, so those citations resolve for a public reader. The rule is about the trees that are
    /// held back, not about citing files at all.
    #[test]
    fn no_row_cites_a_path_the_mirror_withholds() {
        const WITHHELD: [&str; 5] = ["scripts/", "docs/", ".github/", "content/", "justfile"];
        let cites_withheld = |s: &str| {
            WITHHELD.iter().any(|w| {
                s.match_indices(w).any(|(i, _)| {
                    // Only a CITATION counts — the prefix inside a backtick span. Prose that happens
                    // to contain the word "docs/" in running text is not a link anyone follows.
                    s[..i].rfind('`').is_some_and(|b| !s[b + 1..i].contains('`'))
                })
            })
        };
        let mut bad: Vec<String> = Vec::new();
        for f in FLAGS.iter() {
            for (what, s) in [
                ("evidence", f.evidence),
                ("short", f.short),
                ("reason", f.default_reason.unwrap_or("")),
            ] {
                if cites_withheld(s) {
                    bad.push(format!("{} {what}: {s}", f.long));
                }
            }
            for r in f.refusals.iter() {
                if cites_withheld(r.message) {
                    bad.push(format!("{} refusal on {}: {}", f.long, r.on, r.message));
                }
            }
        }
        for r in ROSTERS.iter() {
            for (what, s) in [
                ("evidence", r.evidence),
                ("derived_from", r.derived_from.unwrap_or("")),
                ("admission", r.admission.unwrap_or("")),
            ] {
                if cites_withheld(s) {
                    bad.push(format!("roster {} {what}: {s}", r.id));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "these rows cite a path the public mirror withholds, and this table is published:\n  {}\n\
             Reword the citation — name the thing rather than its path — or the docs site renders a \
             dead link and its own citation gate fails a build in another repository.",
            bad.join("\n  ")
        );
    }

    /// The rendered document is valid JSON carrying the sets this export promises.
    #[test]
    fn the_rendered_document_is_well_formed() {
        let files = rendered_files();
        let raw = files.get(CLI_JSON).expect("cli.json is rendered");
        let doc: serde_json::Value = serde_json::from_str(raw).expect("cli.json is valid JSON");
        assert_eq!(doc["schema_version"], SCHEMA_VERSION);
        assert_eq!(doc["plane"], PLANE);
        assert_eq!(doc["flags"].as_array().map(Vec::len), Some(FLAGS.len()));
        assert_eq!(doc["rosters"].as_array().map(Vec::len), Some(ROSTERS.len()));
        assert!(raw.ends_with('\n'), "a published text asset ends with a newline");
    }

    /// ⚠ **The committed fixture is the frozen schema, and the render must still equal it.**
    ///
    /// Read at RUN TIME rather than through `include_str!`, deliberately: the fixture ships to the
    /// public mirror, and an `include_str!` of a path the mirror could ever withhold makes the whole
    /// mirror fail to COMPILE. That has happened twice in this tree, which is why the run-time read
    /// is the idiom here.
    ///
    /// Compares the FLAG and ROSTER arrays rather than the whole document, because those are the
    /// parts a later stage renders from; the envelope is pinned by the test above.
    #[test]
    fn the_render_equals_the_committed_fixture() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cli.json");
        let committed = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!("the committed fixture is missing at {path} ({e}) — it is the frozen schema this table is written against")
        });
        let files = rendered_files();
        let rendered = files.get(CLI_JSON).expect("cli.json is rendered");
        let a: serde_json::Value =
            serde_json::from_str(&committed).expect("the committed fixture is valid JSON");
        let b: serde_json::Value =
            serde_json::from_str(rendered).expect("the render is valid JSON");
        assert_eq!(
            a.get("flags"),
            b.get("flags"),
            "the render and the committed fixture disagree about the flag table — re-render the fixture, or fix the table"
        );
        assert_eq!(
            a.get("rosters"),
            b.get("rosters"),
            "the render and the committed fixture disagree about the rosters"
        );
    }
}
