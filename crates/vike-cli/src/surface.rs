//! **The published command surface, as DATA.** `cli.json` carries two things side by side:
//!
//! * `commands` — every verb of `vike-cli`, walked off its `clap` tree (`commands`'s `rows`), so a
//!   flag cannot be declared and not published. Nothing here is written by hand.
//! * `flags`, `rosters` and the envelope — the `backtest` plane as it has been published since
//!   before `clap`. A released field is never renamed or dropped, only added to, so these stay; a
//!   row of [`FLAGS`] now holds what `clap` cannot know about a flag (where a refusal reaches, which
//!   side validates a value, the profile key it writes, the default's reason) and the published
//!   wording `clap` spells differently (`short`, `evidence`, a `value_name` such as `host:port`).
//!   `FlagRow::value`, `FlagRow::repeat`, a `value_name` the row does not override, `--help`'s and
//!   `--version`'s sub-verbs (`FlagRow::accepted_on`), `sub_verbs` and the `all_subcommands` roster
//!   come from the command; every other roster reads the const, enum or table it publishes
//!   ([`ROSTERS`]).
//!   `surface_tests.rs`'s `every_flag_of_the_plane_is_classified_exactly_once` holds the rows and the
//!   command's flags equal in both directions; [`FlagRow`]'s doc says why each field it still holds
//!   is not read off the command.
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
//! That is the strongest argument for this shape, and the precedent is `crates/vike-mcp/src/tests/manifest_and_skills.rs`'s
//! `the_registry_manifest_lists_every_tool_this_server_serves`, which compares a committed,
//! externally-published manifest to the live registry the same way.
//!
//! ⚠ **This module is DATA and carries no argument.** Rationale belongs in the module docs of the
//! code it describes and in `crates/vike-cli/CLAUDE.md`; a row here carries what a renderer needs
//! plus the `evidence` naming where it was read from.
//!
//! ⚠ **Key SETS are the contract, never key ORDER.** `serde_json`'s map flavour flips with
//! `preserve_order`, which the workspace's DataFusion crates enabled through feature unification
//! whenever they shared a build (up to datafusion 55.1.0; nothing in `Cargo.lock` enables it
//! since 55.2.0). `vike-cli` is DataFusion-free by construction, so a standalone run was stable
//! while a shared build was not — the divergence was exactly gate-versus-release, and a future
//! dependency that turns the feature on would reopen it.
//! Everything order-bearing here is an ARRAY carrying an explicit name field.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use vike_backtesthub_client::client::DialRefusalId;

use crate::refusals::FlagRefusalId;
use RefusalText::{Clap, Copied, Lent, Raised};

pub(crate) mod commands;

/// The file name this module publishes, and the name the far side's asset list must carry.
///
/// ⚠ It reaches that list as a BARE LITERAL, because `vike-docs` — which owns the gate holding the
/// rendered asset set equal to the release workflow's and the mirror's
/// (`crates/vike-docs/tests/docs_data_gate.rs`'s `expected_docs_data_assets`) — does not link this
/// crate. A normal edge is refused by rank (`crates/vike-cli/Cargo.toml` and that crate's manifest both
/// declare layer 35, so `crates/vike-ops/tests/architecture/layer_gate.rs` refuses it), and a test-only dev edge,
/// which that gate exempts, would compile this whole library into that crate's tests to read one
/// string. That is the same declared hole `bins.json` already carries, and it is recorded here
/// rather than closed. (This said `vike-ops` owns the gate and that this crate "declares the top
/// layer" until 2026-09-28: the gate moved to `vike-docs` with the exporter on 2026-09-26, and this
/// crate declared 30 from the 2026-09-28 re-rank until 2026-11-06.)
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

/// The sub-verb roster, in the order the plane's `clap` command declares it.
///
/// ⚠ **The sub-verb is always required** — there is no bare `vike-cli backtest …` that means `run`.
pub(crate) fn sub_verbs() -> Vec<String> {
    crate::cmd::backtest::command()
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set())
        .map(|sub| sub.get_name().to_string())
        .collect()
}

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
/// ⚠ **`refused` replaced `last_wins`, and the published word moved with the behaviour.** Under the
/// hand-rolled loops `--profile a.toml --profile b.toml` ran `b.toml` with no warning; `clap` refuses
/// a flag given twice unless it is declared repeatable (decision 0155), so those rows published a
/// behaviour the binary no longer had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    /// A value-taking flag `clap` refuses when it is given twice.
    Refused,
    /// Every occurrence is kept.
    Accumulate,
    /// Repeatable AND comma-splitting, folded into ONE override after the parse, in argv order.
    CommaListFold,
    /// A bare switch: repeating discards no earlier value and changes no outcome.
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
/// on every other reading verb, and is accepted on `run` — one flag, three answers.
#[derive(Debug, Clone, Copy)]
pub struct Refusal {
    /// The sub-verb (or the condition) this message answers.
    pub on: &'static str,
    /// Whose words the message is, and so where it is rendered from.
    text: RefusalText,
}

/// The words of one published refusal.
#[derive(Debug, Clone, Copy)]
enum RefusalText {
    /// Raised by this crate: the variant's template (`crate::refusals`), each hole named here filled
    /// with the value beside it, every other hole left as its template. Published with the id.
    Raised(FlagRefusalId, &'static [(&'static str, &'static str)]),
    /// `clap`'s own words: this line of the plane (the words after `vike-cli backtest`) is run
    /// through its parser and the first line of the refusal published, so the row cannot drift
    /// from what `clap` prints. Published with no id: the words are `clap`'s.
    Clap(&'static [&'static str]),
    /// Words another crate, the far side or a product line owns, copied VERBATIM; a `format!` hole
    /// is written as its template. Published with no id.
    Copied(&'static str),
    /// A refusal a crate BELOW this one declares (the one copy `vike-cli` and `vike-mcp` both show): its
    /// id's `word()` and `template()`, read off that id. Published with the id.
    Lent(&'static str, &'static str),
}

impl Refusal {
    /// The published message: the variant's template with the row's holes filled, or the copy.
    pub fn message(&self) -> String {
        match self.text {
            Raised(id, fills) => {
                let mut text = id.template().to_string();
                for (hole, value) in fills {
                    text = text
                        .replace(&format!("{{{hole}}}"), value)
                        .replace(&format!("{{{hole}:?}}"), value);
                }
                // A literal brace is doubled in a `format!` template; a reader gets it once.
                text.replace("{{", "{").replace("}}", "}")
            }
            // Empty when the line parses, which `every_refusal_carries_a_message` refuses.
            Clap(line) => crate::cmd::backtest::parse_refusal(line)
                .map(|text| {
                    let text = text.strip_prefix("vike-cli backtest: ").unwrap_or(&text);
                    text.lines().next().unwrap_or_default().to_string()
                })
                .unwrap_or_default(),
            Copied(message) => message.to_string(),
            Lent(_, template) => template.replace("{{", "{").replace("}}", "}"),
        }
    }

    /// The refusal's id, when this crate raises it.
    pub(crate) fn id(&self) -> Option<FlagRefusalId> {
        match self.text {
            Raised(id, _) => Some(id),
            Clap(_) | Copied(_) | Lent(..) => None,
        }
    }

    /// The id word published beside the message: this crate's or a lent one's.
    fn id_word(&self) -> Option<&'static str> {
        match self.text {
            Lent(word, _) => Some(word),
            _ => self.id().map(FlagRefusalId::word),
        }
    }
}

/// One flag: what `clap` cannot say about it. Every row is keyed by [`Self::long`], and
/// `surface_tests.rs`'s `every_flag_of_the_plane_is_classified_exactly_once` holds the rows and the
/// plane's `clap` flags equal in both directions. Why each field is not read off the command:
///
/// - `sample`: `clap` declares no example value.
/// - `applies_to`, `refusal_reaches`, `status`: the reading verbs share ONE `clap` flag set and
///   refuse a foreign flag BY NAME after the parse (`crate::cmd::backtest::read`'s
///   `refuse_foreign_read_flags`), and an unbuilt or retired flag is a hidden argument refused by
///   name, so `clap` accepts flags the binary refuses.
/// - `value_name` (where it differs) and `short`: the released wording; moving it into `clap` would
///   change `--help` (`<p.toml>` prints as `<<p.toml>>` there).
/// - `roster_id`, `validated_by`, `profile_key`, `shape` and the `default_*` fields: what the PROFILE
///   field a flag writes declares, so they belong to the profile surface, not to `clap`.
/// - `refusals`: the binary's sentences, copied; their producers are the binary's refusal sites.
/// - `mode_scope`, `evidence`: no code holds them.
#[derive(Debug, Clone, Copy)]
pub struct FlagRow {
    pub long: &'static str,
    /// The published metavar where it differs from the one the `clap` field declares, which is what
    /// `None` publishes. It is kept because a released value does not change wording.
    pub value_name: Option<&'static str>,
    /// A value that PARSES today. This is what retires the two hand-written arity tables.
    pub sample: Option<&'static str>,
    /// The sub-verbs that ACCEPT it; empty for `--help` and `--version`, whose sub-verbs
    /// `FlagRow::accepted_on` reads off the command.
    ///
    /// ⚠ `clap` cannot say this: the reading family declares ONE flag set on all its sub-verbs and
    /// refuses a foreign flag after the parse (`crate::cmd::backtest::read`'s `ReadCommand`).
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

impl FlagRow {
    /// The plane's `clap` argument this row describes: the visible declaration when there is one
    /// (`--cash` is visible on `run` and hidden on the reading verbs), else the hidden one. `None`
    /// for `--help` and `--version`, which `clap` adds when it builds the command.
    fn clap_arg(&self) -> Option<clap::Arg> {
        let long = self.long.trim_start_matches("--");
        let plane = crate::cmd::backtest::command();
        let declared: Vec<&clap::Arg> = plane
            .get_arguments()
            .chain(plane.get_subcommands().flat_map(clap::Command::get_arguments))
            .filter(|arg| arg.get_long() == Some(long))
            .collect();
        declared
            .iter()
            .find(|arg| !arg.is_hide_set())
            .or_else(|| declared.first())
            .map(|arg| (*arg).clone())
    }

    /// Whether the flag takes a value, off its `clap` action.
    ///
    /// A `num_args(0..=1)` registration only swallows what was typed beside a flag that refuses
    /// anyway, so on an unbuilt row it is no value; the retired `--store` took a DIR, and is
    /// published as taking one.
    pub(crate) fn value(&self) -> Value {
        let Some(arg) = self.clap_arg() else { return Value::None };
        let optional = arg.get_num_args().is_some_and(|range| range.min_values() == 0);
        if arg.get_action().takes_values() && (!optional || self.status == Status::Retired) {
            Value::Required
        } else {
            Value::None
        }
    }

    /// The sub-verbs that accept the flag, as published: [`Self::applies_to`], but for `--help` and
    /// `--version`, which no field of ours declares — `clap` adds them to each sub-verb's command
    /// when it builds them, so their sub-verbs are read off the BUILT command.
    pub(crate) fn accepted_on(&self) -> Vec<String> {
        if self.clap_arg().is_some() {
            return self.applies_to.iter().map(|sub| (*sub).to_string()).collect();
        }
        let long = self.long.trim_start_matches("--");
        let mut plane = crate::cmd::backtest::command();
        plane.build();
        plane
            .get_subcommands()
            .filter(|sub| !sub.is_hide_set())
            .filter(|sub| sub.get_arguments().any(|arg| arg.get_long() == Some(long)))
            .map(|sub| sub.get_name().to_string())
            .collect()
    }

    /// What repeating the flag does, off its `clap` action, but for the two answers an action
    /// cannot give: `--symbol` folds into one override after the parse, and `--store` is refused by
    /// name however often it is given.
    pub(crate) fn repeat(&self) -> Repeat {
        match self.long {
            "--symbol" => return Repeat::CommaListFold,
            "--store" => return Repeat::Idempotent,
            _ => {}
        }
        let append =
            self.clap_arg().is_some_and(|arg| matches!(arg.get_action(), clap::ArgAction::Append));
        match (self.value(), append) {
            (Value::None, _) => Repeat::Idempotent,
            (Value::Required, true) => Repeat::Accumulate,
            (Value::Required, false) => Repeat::Refused,
        }
    }

    /// The metavar `cli.json` publishes.
    fn published_value_name(&self) -> Option<String> {
        self.value_name.map(str::to_owned).or_else(|| self.clap_value_name())
    }

    /// The metavar the `clap` field declares. (The derive names one on a bare switch too.)
    fn clap_value_name(&self) -> Option<String> {
        let arg = self.clap_arg().filter(|arg| arg.get_action().takes_values())?;
        arg.get_value_names()?.first().map(ToString::to_string)
    }
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

/// Every flag on the plane, one row each. A `static`, not a `const`: at 59 rows the table passed clippy's
/// `large_const_arrays` bound, and a `const` would be copied into every use.
pub static FLAGS: [FlagRow; 63] = [
    FlagRow {
        long: "--add",
        value_name: None,
        sample: Some("ci"),
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
        short: "`tag` only, and THE ONLY REPEATABLE FLAG IN THE READING FLAG LOOP — `--add ci --add fee-fix` is two labels, pushed onto `ReadArgs::add`. Writes into the run's own META_FILE (meta.json) sidecar via `vike_runs::add_tags`.",
        evidence: "crates/vike-cli/src/cmd/backtest/read.rs's `ReadCommand::add` (`ArgAction::Append`, with the ⚠ comment naming it the only repeatable flag of the reading verbs), which `ReadCommand::into_args` hands to `ReadArgs::add`; crates/vike-cli/src/cmd/backtest/read/tag.rs's `run_tag`",
        refusals: &[
            Refusal {
                on: "ls, show, path, diff, gate, params, strategies, templates, script-api, script-check, prune",
                text: Raised(FlagRefusalId::OnlyOnTag, &[("flag", "--add")]),
            },
            Refusal {
                on: "tag, with none of --add, --note and --as",
                text: Raised(FlagRefusalId::TagWritesNothing, &[]),
            },
        ],
    },
    FlagRow {
        long: "--addr",
        value_name: Some("host:port"),
        sample: Some("127.0.0.1:7880"),
        // ⚠ `strategies` was missing, and the (flag, sub-verb) gate found it: `parse_read`'s own
        // `"--addr"` arm accepts it, and `refuse_foreign_read_flags` refuses it on every reading
        // verb EXCEPT that one — because `strategies` is the single reading verb that dials the
        // backtesthub rather than reading a run directory on this machine. A table that omits
        // the one verb a flag exists for is worse than one that omits nothing at all.
        applies_to: &["run", "strategies"],
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
            "`resolve_addr(cli, configured)` folds three rungs and falls through to `vike_config::DEFAULT_BACKTEST_ADDRESS`. The FLAG layer performs no host:port check at all (a blank value is treated as absent, and dialling anything else produces a connect-rung failure); the settings row does check, via `vike_config`'s `check_addr`.",
        ),
        short: "The backtesthub to dial (vike-backend backtesthub --addr, NOT the datahub). Refused beside --local; unwritten, it falls through config.backtest_addr to 127.0.0.1:7880.",
        evidence: "crates/vike-client/src/resolved.rs's `backtesthub_dial_address` and `Resolved::backtest_addr`; crates/vike-cli/src/cmd/backtest.rs's `Args::addr` and crates/vike-cli/src/cmd/backtest/execute.rs's `execute`; crates/vike-config/src/config.rs's `DEFAULT_BACKTEST_ADDRESS`, `Config::backtest_addr` and `check_address`",
        refusals: &[
            Refusal {
                on: "--addr combined with --local (CLI, exit 2)",
                text: Raised(FlagRefusalId::AddrBesideLocal, &[]),
            },
            Refusal {
                on: "the daemon is not reachable (exit 3, the CONNECT rung; `{addr}` is the resolved address and `{e}` the transport error)",
                text: Lent(
                    DialRefusalId::CannotConnect.word(),
                    DialRefusalId::CannotConnect.template(),
                ),
            },
            Refusal { on: "written with no value", text: Clap(&["run", "--addr"]) },
            Refusal {
                on: "ls, show, path, tag, diff, gate, params, templates, script-api, script-check, prune",
                text: Raised(FlagRefusalId::OnlyOnStrategies, &[("flag", "--addr")]),
            },
        ],
    },
    FlagRow {
        long: "--against",
        value_name: Some("<mark>"),
        sample: Some("@baseline/momentum"),
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
        evidence: "crates/vike-cli/src/cmd/backtest/read/gate.rs's `refuse_an_ungateable_line` and `run_gate`",
        refusals: &[
            Refusal {
                on: "gate, absent or blank",
                text: Raised(FlagRefusalId::AgainstRequired, &[]),
            },
            Refusal {
                on: "ls, show, path, tag, diff, params, strategies, templates, script-api, script-check, prune",
                text: Raised(FlagRefusalId::OnlyOnGate, &[("flag", "--against")]),
            },
        ],
    },
    FlagRow {
        long: "--all",
        value_name: None,
        sample: None,
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
        short: "`diff` only. Shows UNCHANGED leaves too — the negation of the default. Read by `run_diff` as `show_all` and passed to all three renderers (human/markdown/json).",
        evidence: "crates/vike-cli/src/cmd/backtest/read/diff.rs's `run_diff` (`let show_all = a.all;`)",
        refusals: &[
            Refusal {
                on: "ls, show, path, tag, gate, params, strategies, templates, script-api, script-check, prune",
                text: Raised(FlagRefusalId::OnlyOnDiff, &[("flag", "--all")]),
            },
            Refusal {
                on: "(any reading sub-verb, with a value)",
                text: Clap(&["diff", "--all={value}"]),
            },
        ],
    },
    FlagRow {
        long: "--as",
        value_name: None,
        sample: Some("baseline/momentum"),
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
        short: "`tag` only. Sets a MARK — the stable second operand `gate --against` takes — in a file that is a SIBLING of the runs root (`vike_paths::state_path::MARKS_SUBDIR`). Validated at PARSE time by `vike_runs::valid_mark_name`, whose message is prefixed with `--as `.",
        evidence: "crates/vike-cli/src/cmd/backtest/read/tag.rs's `refuse_a_tag_that_writes_nothing` (the `valid_mark_name(name).map_err(|why| format!(\"--as {why}\"))` call) and `run_tag`; crates/vike-runs/src/lib.rs's `valid_mark_name`",
        refusals: &[
            Refusal {
                on: "tag, empty name",
                text: Copied(
                    "--as a mark name is required (for example `baseline/momentum`, or `prod`)",
                ),
            },
            Refusal {
                on: "tag, leading/trailing whitespace",
                text: Copied("--as '{name}': a mark name may not begin or end with whitespace"),
            },
            Refusal {
                on: "tag, more than 3 '/'-separated parts",
                text: Copied(
                    "--as '{name}': a mark name may have at most 3 '/'-separated parts — a mark is a label, not a directory tree",
                ),
            },
            Refusal {
                on: "tag, an empty part",
                text: Copied(
                    "--as '{name}': an empty part — a mark name may not start or end with '/' and may not contain '//'",
                ),
            },
            Refusal {
                on: "tag, a part starting with '.'",
                text: Copied(
                    "--as '{name}': the part '{segment}' starts with '.' — that is a traversal ('..'), or a dot-entry every directory scan in this workspace skips, so the mark would be written and then be invisible",
                ),
            },
            Refusal {
                on: "tag, a disallowed character",
                text: Copied(
                    "--as '{name}': the part '{segment}' contains '{bad}' — a mark name takes ASCII letters, digits, '.', '_', '-' and '/' only, because it becomes a file path on every platform this ships to",
                ),
            },
            Refusal {
                on: "ls, show, path, diff, gate, params, strategies, templates, script-api, script-check, prune",
                text: Raised(FlagRefusalId::OnlyOnTag, &[("flag", "--as")]),
            },
        ],
    },
    FlagRow {
        long: "--attribution",
        value_name: None,
        sample: None,
        applies_to: &[],
        refusal_reaches: &[
            "ls",
            "show",
            "path",
            "tag",
            "diff",
            "gate",
            "params",
            "strategies",
            "templates",
            "script-api",
            "script-check",
            "prune",
        ],
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
        short: "Refused by name on any reading sub-verb: the engine computes no per-source attribution at all, so this is missing DATA rather than a missing renderer.",
        evidence: "`crates/vike-cli/src/cmd/backtest/read/show.rs`'s `refuse_an_unbuilt_renderer` (`\"--attribution\"` arm); roster `UNBUILT_RENDERERS`.",
        refusals: &[Refusal {
            on: "any use, on any reading sub-verb",
            text: Raised(FlagRefusalId::AttributionUnbuilt, &[]),
        }],
    },
    FlagRow {
        long: "--breakdown",
        value_name: None,
        sample: None,
        applies_to: &[],
        refusal_reaches: &[
            "ls",
            "show",
            "path",
            "tag",
            "diff",
            "gate",
            "params",
            "strategies",
            "templates",
            "script-api",
            "script-check",
            "prune",
        ],
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
        short: "Refused by name on any reading sub-verb, and UNCONDITIONALLY: a period breakdown needs per-bar returns, and the reason it is refused — a curve DECIMATED past the sample cap — holds only for a large run, while this guard sees argv alone and no run. `vike-cli report run:<run> --breakdown day|month` evaluates that condition per run, off the same series.json.",
        evidence: "`crates/vike-cli/src/cmd/backtest/read/show.rs`'s `refuse_an_unbuilt_renderer` (`\"--breakdown\"` arm); roster `UNBUILT_RENDERERS`.",
        refusals: &[Refusal {
            on: "any use, on any reading sub-verb",
            text: Raised(FlagRefusalId::BreakdownUnbuilt, &[]),
        }],
    },
    FlagRow {
        long: "--cash",
        value_name: None,
        sample: Some("10000"),
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: Some("engine.cash"),
        shape: Some("scalar (TOML-typed via parse_scalar)"),
        mode_scope: "any",
        default_kind: "implied",
        default_value: Some("10000.0"),
        default_conditional_on: None,
        default_reason: Some("no --cash and no profile said otherwise"),
        short: "A `run`-only sugar flag (SUGAR row `engine.cash`, Shape::Scalar) included here because it has THREE different answers across this plane. ACCEPTED on `run`. On `params` it is on `PARAMS_REFUSED` and gets the run-flag sentence. On every other reading sub-verb it is not a known token at all and falls into the generic unknown-option arm.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's `SUGAR` (the `Sugar { flag: \"--cash\", key: \"engine.cash\", shape: Shape::Scalar }` row), `PARAMS_REFUSED`, `refuse_a_run_flag_on_params`, and crates/vike-cli/src/cmd/backtest/read.rs's `HiddenRefusals` (which declares it hidden on every reading sub-verb) and `ReadCommand::into_args` (`unknown option '{flag}'` off `params`); crates/vike-cli/src/cmd/backtest/run_args.rs's `implied_defaults`",
        refusals: &[
            Refusal {
                on: "params",
                text: Raised(FlagRefusalId::RunFlagOnParams, &[("flag", "--cash")]),
            },
            Refusal {
                on: "ls, show, path, tag, diff, gate, strategies, templates, script-api, script-check, prune",
                text: Raised(FlagRefusalId::UnknownOption, &[("flag", "--cash")]),
            },
            Refusal {
                on: "run",
                text: Copied(
                    "(ACCEPTED — no refusal. It pushes an Override with key `engine.cash`, value `parse_scalar(raw)`, origin `Origin::Sugar(\"--cash\")`.)",
                ),
            },
        ],
    },
    FlagRow {
        long: "--changed-only",
        value_name: None,
        sample: None,
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
        evidence: "crates/vike-cli/src/cmd/backtest/read.rs's `ReadArgs::changed_only` (its doc: \"the DEFAULT, accepted so a script that says what it means is not refused\"); crates/vike-cli/src/cmd/backtest/read/diff.rs's `run_diff` never reads the field",
        refusals: &[
            Refusal {
                on: "ls, show, path, tag, gate, params, strategies, templates, script-api, script-check, prune",
                text: Raised(FlagRefusalId::OnlyOnDiff, &[("flag", "--changed-only")]),
            },
            Refusal {
                on: "(any reading sub-verb, with a value)",
                text: Clap(&["diff", "--changed-only={value}"]),
            },
        ],
    },
    FlagRow {
        long: "--cols",
        value_name: None,
        sample: Some("run_id,kind,sharpe"),
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
        evidence: "crates/vike-cli/src/cmd/backtest/read/ls.rs's `run_ls` (the `a.cols` split) and `DEFAULT_COLS`",
        refusals: &[
            Refusal {
                on: "ls, when every element is empty after trimming",
                text: Raised(FlagRefusalId::ColsNoNames, &[]),
            },
            Refusal {
                on: "show, path, tag, diff, gate, params, strategies, templates, script-api, script-check, prune",
                text: Raised(FlagRefusalId::OnlyOnLs, &[("flag", "--cols")]),
            },
        ],
    },
    FlagRow {
        long: "--config",
        value_name: None,
        sample: None,
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
        evidence: "crates/vike-cli/src/cmd/backtest/read/show.rs's `show_text` (the `config || all` branch) and that module's doc `⚠ --config says what is NOT recorded`",
        refusals: &[
            Refusal {
                on: "ls, path, tag, diff, gate, params, strategies, templates, script-api, script-check, prune",
                text: Raised(FlagRefusalId::OnlyOnShow, &[("flag", "--config")]),
            },
            Refusal {
                on: "(any reading sub-verb, with a value)",
                text: Clap(&["show", "--config={value}"]),
            },
        ],
    },
    FlagRow {
        long: "--decide",
        value_name: Some("mode"),
        sample: Some("sequential"),
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: Some("decide_modes"),
        validated_by: "cli",
        profile_key: Some("engine.decide"),
        shape: Some(
            "one of `sequential` | `simultaneous` (the `decide_modes` roster, ASCII case folded as the engine folds it), forwarded VERBATIM once accepted. The roster is `vike_model::run_request::decide::DecideMode`, the one home the engine resolves too, so this local check is no copy; a profile FILE's `engine.decide` is still refused only by the engine.",
        ),
        mode_scope: "any",
        default_kind: "far_side",
        default_value: Some("sequential"),
        default_conditional_on: None,
        default_reason: Some(
            "This crate substitutes NO default — an absent --decide writes no `engine.decide` key at all, and `decide_mode` resolves an absent key to `DecideMode::Sequential`, byte-identical to every profile written before the key existed. It is NOT an `implied_defaults` row for that reason: there is nothing the schema failed to write down.",
        ),
        short: "WHEN a multi-symbol step is decided. `simultaneous` makes the run's answer a property of the instrument SET rather than of the order data.symbols was typed in, and IMPLIES engine.cash_gate. Sugar for `--set engine.decide=…`, reaching the same key through the same applier.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's `SUGAR` (the `engine.decide` row) and `RunCommand::into_args` (the `Shape::Str` arm's `DecideMode::parse` check); crates/vike-model/src/run_request/decide.rs's `DecideMode`; crates/vike-backtest/src/run/profile/engine_config.rs's `ProfileEngine::decide` and crates/vike-backtest/src/run/profile.rs's `decide_mode`",
        refusals: &[
            Refusal {
                on: "run, a value outside decide_modes (this side, before any spawn or dial)",
                text: Raised(
                    FlagRefusalId::ValueNotInRoster,
                    &[("flag", "--decide"), ("roster", "sequential|simultaneous")],
                ),
            },
            Refusal {
                on: "a profile's own engine.decide spelled outside the roster (the ENGINE or the backtesthub, after a spawn or a dial)",
                text: Copied(
                    "unknown engine.decide {other:?} (want sequential | simultaneous; absent = sequential, the per-symbol walk in the order data.symbols lists them)",
                ),
            },
            Refusal {
                on: "`simultaneous` on a tick profile (the far side, ProfileBacktest::refusals)",
                text: Copied(
                    "engine.decide = \"simultaneous\" is bar-mode only: the cross-sectional …",
                ),
            },
            Refusal {
                on: "`simultaneous` beside an armed [risk] table or engine.leverage (the far side, ProfileBacktest::refusals) — a DECLARED GAP rather than a preference: with a pre-trade gate armed the symbol judged first faces the whole step budget, and the fix is a step-boundary snapshot that is not built",
                text: Copied(
                    "engine.decide = \"simultaneous\" is not built for a run with a pre-trade risk gate: remove [risk] and engine.leverage, or keep engine.decide …",
                ),
            },
            Refusal {
                on: "params",
                text: Raised(FlagRefusalId::RunFlagOnParams, &[("flag", "--decide")]),
            },
        ],
    },
    FlagRow {
        long: "--drawdowns",
        value_name: None,
        sample: None,
        applies_to: &[],
        refusal_reaches: &[
            "ls",
            "show",
            "path",
            "tag",
            "diff",
            "gate",
            "params",
            "strategies",
            "templates",
            "script-api",
            "script-check",
            "prune",
        ],
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
        short: "Refused by name on any reading sub-verb, and what is missing is the WIRING rather than the renderer: `vike_analytics::periods::drawdown_table_text` is linked into this binary and nothing calls it, while the equity curve it reads is in the run record and `--export equity` hands it over today.",
        evidence: "`crates/vike-cli/src/cmd/backtest/read/show.rs`'s `refuse_an_unbuilt_renderer` (`\"--drawdowns\"` arm); roster `UNBUILT_RENDERERS`.",
        refusals: &[Refusal {
            on: "any use, on any reading sub-verb",
            text: Raised(FlagRefusalId::DrawdownsUnbuilt, &[]),
        }],
    },
    FlagRow {
        long: "--dry-run",
        value_name: None,
        sample: None,
        applies_to: &["prune"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: None,
        shape: Some("bool"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some("REQUIRED by `prune`, which removes nothing."),
        short: "`prune` only, and required there. Lists the saved runs the saved-run retention (`config.saved_runs_per_strategy`) would remove: per strategy file the backtests over the newest N, never a run of another kind, a marked, tagged or noted run, or one another run or the alert rules name. Nothing is removed; the backtesthub removes them after its next save.",
        evidence: "crates/vike-cli/src/cmd/backtest/read/prune.rs's `run_prune`; crates/vike-cli/src/cmd/backtest/read.rs's `ReadArgs::dry_run`; crates/vike-runs/src/retention.rs's `RunRetention`",
        refusals: &[
            Refusal {
                on: "prune, without it",
                text: Raised(FlagRefusalId::PruneWithoutDryRun, &[]),
            },
            Refusal {
                on: "ls, show, path, tag, diff, gate, params, strategies, templates, script-api, script-check",
                text: Raised(FlagRefusalId::OnlyOnPrune, &[("flag", "--dry-run")]),
            },
            Refusal {
                on: "(any reading sub-verb, with a value)",
                text: Clap(&["prune", "--dry-run={value}"]),
            },
        ],
    },
    FlagRow {
        long: "--engine",
        value_name: None,
        sample: Some("/opt/backtest"),
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
            "<project>/bin/backtesthub[EXE] → <exe_dir>/backtesthub[EXE] → <project>/bin/vike-backend[EXE] with lead arg `backtesthub` → <exe_dir>/vike-backend[EXE] with lead arg `backtesthub` → bare `backtesthub` on PATH",
        ),
        default_conditional_on: None,
        default_reason: Some(
            "`crate::cmd::engine`'s `locate(explicit, project_root)`: an explicit path short-circuits the whole ladder and is deliberately NOT probed for existence (a named path that is not there must fail with the name the operator typed), and is never treated as a multicall. Otherwise the four-rung standalone-then-multicall search runs, with bare `backtest` on PATH as the last word.",
        ),
        short: "Name the standalone engine outright instead of searching for it. Local-arm only; never probed for existence.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `Args::engine`, crates/vike-cli/src/cmd/backtest/run_args.rs's `RunCommand::into_args`'s remote-arm refusal and crates/vike-cli/src/cmd/backtest/execute.rs's `execute_local`; crates/vike-cli/src/cmd/engine.rs's `locate`, `ENGINE_BIN` and `cannot_spawn`",
        refusals: &[
            Refusal {
                on: "⚠ typed WITHOUT --local, i.e. on the remote arm (CLI, exit 2). One line, no continuations.",
                text: Raised(FlagRefusalId::EngineWithoutLocal, &[]),
            },
            Refusal {
                on: "the named program cannot be spawned (CONNECT rung; `{}` is the engine display form, `{e}` the io::Error, `MISSING_HINT` a constant in crates/vike-cli/src/cmd/engine.rs that this area did not transcribe)",
                text: Raised(FlagRefusalId::CannotSpawn, &[("title", "the backtesthub engine")]),
            },
            Refusal { on: "written with no value", text: Clap(&["run", "--engine"]) },
        ],
    },
    FlagRow {
        long: "--euler-depth",
        value_name: None,
        sample: Some("2"),
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
            "CONFIRMED forwarded verbatim (same three sites as --trials). Default applied only under euler, by `search::select`'s `parse_euler_depth` → `SearchEulerConfig::DEFAULT_MAX_DEPTH` = 3; the hard cap is `SearchEulerConfig::MAX_DEPTH_CAP` = 16 and 0 is legal (coarse-grid-only).",
        ),
        short: "euler's successive-halving depth. Forwarded as a token; range-checked (0..=16) by the engine, never clamped silently.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `Args::euler_depth` and crates/vike-cli/src/cmd/backtest/run_args.rs's `RunCommand::euler_depth`, which `RunCommand::into_args` forwards as typed; crates/vike-backtest/src/search/select.rs's `parse_euler_depth`; crates/vike-backtest/src/search.rs's `SearchEulerConfig::DEFAULT_MAX_DEPTH` and `SearchEulerConfig::MAX_DEPTH_CAP`",
        refusals: &[
            Refusal {
                on: "--euler-depth under any method but euler (from METHOD_KNOBS, both routes)",
                text: Copied(
                    "--euler-depth is a euler flag, but this run selected the \"grid\" optimizer. It was silently discarded before; it is refused now, because a knob that configures a search you did not select cannot do what it says",
                ),
            },
            Refusal {
                on: "a value past the cap, under euler (`{d}` is the parsed value, the second hole is SearchEulerConfig::MAX_DEPTH_CAP)",
                text: Copied(
                    "--euler-depth 99 is past the cap of 16 — it was silently clamped before, which made the budget line report a depth you did not ask for",
                ),
            },
            Refusal {
                on: "a non-integer value, under euler",
                text: Copied("invalid --euler-depth \"abc\" (expected an integer)"),
            },
            Refusal {
                on: "written with no value (`clap`, on this side, before any spawn or dial)",
                text: Clap(&["run", "--euler-depth"]),
            },
        ],
    },
    FlagRow {
        long: "--export",
        value_name: Some("trades|equity"),
        sample: Some("trades"),
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
        evidence: "crates/vike-cli/src/cmd/backtest/read/show.rs's `export_document` and `EXPORTABLE`; `run_show`'s `match &a.export`",
        refusals: &[
            Refusal {
                on: "value `fills` (Usage, rung 2) — refused at EXECUTION time in `export_document`, not at parse time",
                text: Raised(FlagRefusalId::ExportFills, &[]),
            },
            Refusal {
                on: "any value containing a comma, e.g. `trades,equity` (Usage, rung 2). Template: \"--export '{value}': one value at a time. …\" where {value} is the trimmed spec as typed",
                text: Raised(FlagRefusalId::ExportList, &[("value", "trades,equity")]),
            },
            Refusal {
                on: "any other unrecognised value (Usage, rung 2). Template: \"--export '{other}' is not a value this verb knows — §6.2's set is trades, equity and fills, of which {list} are stored (fills are not: see `--export fills`)\" where {other} is the trimmed value and {list} is `EXPORTABLE.join(\" and \")` = \"trades and equity\"",
                text: Raised(
                    FlagRefusalId::ExportUnknownValue,
                    &[("other", "nonsense"), ("list", "trades and equity")],
                ),
            },
            Refusal {
                on: "`--export trades` on a run whose producer wrote no trades.json (Failed, rung 1). Template holes: {run_id} = the run's directory name, {file} = `vike_runs::TRADES_FILE` = \"trades.json\"",
                text: Raised(
                    FlagRefusalId::ExportNoTradeLedger,
                    &[("run_id", "a-1-0"), ("file", vike_runs::TRADES_FILE)],
                ),
            },
            Refusal {
                on: "`--export equity` on a run whose producer wrote no series.json (Failed, rung 1). Template holes: {run_id} = the run's directory name, {file} = `vike_runs::SERIES_FILE` = \"series.json\"",
                text: Raised(
                    FlagRefusalId::ExportNoEquityCurve,
                    &[("run_id", "a-1-0"), ("file", vike_runs::SERIES_FILE)],
                ),
            },
            Refusal {
                on: "any other RunReadError on either value (Failed, rung 1)",
                text: Copied(
                    "<the `RunReadError`'s own Display, passed through verbatim by `CliError::failed(other.to_string())`>",
                ),
            },
            Refusal {
                on: "used on any sub-verb other than `show` (Usage, rung 2), by `refuse_foreign_read_flags`'s `show_only` roster. Template: \"{flag} does not apply to `backtest {sub}` — {why}\"",
                text: Raised(FlagRefusalId::OnlyOnShow, &[("flag", "--export"), ("sub", "ls")]),
            },
        ],
    },
    FlagRow {
        long: "--fail-if",
        value_name: None,
        sample: Some("sharpe:-5%,max_dd:+10%"),
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
        evidence: "crates/vike-cli/src/cmd/backtest/read/gate.rs's `refuse_an_ungateable_line`; crates/vike-cli/src/cmd/backtest/read/failif.rs's `parse_fail_if` and that module's grammar block",
        refusals: &[
            Refusal {
                on: "the whole expression is empty or whitespace",
                text: Raised(FlagRefusalId::FailIfEmpty, &[]),
            },
            Refusal {
                on: "a term between two commas is empty",
                text: Raised(FlagRefusalId::FailIfEmptyCriterion, &[]),
            },
            Refusal {
                on: "the colon is missing (format! template; {term} is the trimmed term)",
                text: Raised(FlagRefusalId::FailIfNoColon, &[]),
            },
            Refusal {
                on: "the metric before the colon is empty ({term} = the trimmed term)",
                text: Raised(FlagRefusalId::FailIfNoMetric, &[]),
            },
            Refusal {
                on: "no leading + or - on the tolerance ({term} = the trimmed term, {metric} = the trimmed metric)",
                text: Raised(FlagRefusalId::FailIfNoSign, &[]),
            },
            Refusal {
                on: "the magnitude does not parse as f64 ({term} = the trimmed term, {number} = the text after the sign with any trailing % stripped, NOT trimmed)",
                text: Raised(FlagRefusalId::FailIfNotANumber, &[]),
            },
            Refusal {
                on: "the magnitude parses but is not finite, e.g. `sharpe:+inf` ({term}, {number} as above)",
                text: Raised(FlagRefusalId::FailIfNotFinite, &[]),
            },
            Refusal {
                on: "the magnitude is negative, e.g. `sharpe:--5%` ({term} = the trimmed term, {metric} = the trimmed metric)",
                text: Raised(FlagRefusalId::FailIfNegative, &[]),
            },
            Refusal {
                on: "two criteria resolve to the SAME report key — including an alias pair such as `max_dd:+5%,max_drawdown:+10%` ({prev} and {c} are the two criteria re-rendered by Criterion's Display, {key} is the resolved JSON key)",
                text: Raised(FlagRefusalId::FailIfMetricTwice, &[]),
            },
            Refusal {
                on: "gate, absent or blank",
                text: Raised(FlagRefusalId::FailIfRequired, &[]),
            },
        ],
    },
    FlagRow {
        long: "--fee",
        value_name: None,
        // Still a value-taking flag (it is parsed to be answered), so it carries one.
        sample: Some("0.001"),
        // Accepted NOWHERE since the owner's ruling of 2026-11-18: a run's commission is a strategy
        // param, so `run` answers this spelling with its replacement.
        applies_to: &[],
        refusal_reaches: &["run"],
        status: Status::Retired,
        roster_id: None,
        validated_by: "cli",
        profile_key: None,
        shape: Some("retired, so no value of any shape is accepted"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "A retired spelling has no default: it configures nothing on any route. What replaced it is the strategy's `commission` param (`--param commission=<amount>`), whose default is the strategy's own commission, else none.",
        ),
        short: "RETIRED. A run's commission is a strategy parameter now (the owner's ruling of 2026-11-18): --param commission=<amount> with commission_unit, or --set strategy.params.commission. The run-level engine.fee_rate it wrote is refused by the profile loader too.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's `FEE_RETIRED` (answered in `RunCommand::into_args`); crates/vike-backtest/src/run/costs.rs's `refuse_run_level_costs`",
        refusals: &[Refusal {
            on: "run (retired)",
            text: Raised(
                FlagRefusalId::FeeRetired,
                &[("units", "percent|per_contract|per_order|probability_scaled")],
            ),
        }],
    },
    FlagRow {
        long: "--fresh",
        value_name: None,
        sample: None,
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
            "A bare boolean; it decides whether an IDENTICAL run the backtesthub saved may answer. Absent = it may (decision 0078's amendment of 2026-11-20): the same profile, strategy text, artifact and build over unchanged data is answered from the saved run, saying so.",
        ),
        short: "Run the backtest even when the backtesthub saved an IDENTICAL run (same profile, strategy text, artifact and build, its data unchanged) whose answer it would give back at once, marked `answered from saved run <id>`. For a measurement, or a strategy that answers otherwise each time; the run is saved as any other. A backtesthub without the feature recomputes every run anyway; --local always runs.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `Args::fresh`; crates/vike-cli/src/cmd/backtest/execute.rs's execute (`set_fresh` on the client it dials)",
        refusals: &[Refusal {
            on: "an inline value (`--fresh=1`)",
            text: Clap(&["run", "--fresh={value}"]),
        }],
    },
    FlagRow {
        long: "--from",
        value_name: Some("DATE"),
        sample: Some("2026-01-01T00"),
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
            "`ProfileData::from` carries NO serde default in crates/vike-backtest/src/run/profile/data_cfg.rs, so an absent value is a far-side missing-field refusal. The CLI implies nothing.",
        ),
        short: "Sugar for data.from. Shape::Str is REQUIRED not a preference: a bare epoch-ms stays a string, so --from 0 works while --set data.from=0 is a far-side type error.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's SUGAR and its Shape::Str doc; pinned by crates/vike-cli/src/cmd/backtest/tests/builder_sugar_param.rs's a_bare_epoch_ms_date_stays_a_string_through_the_sugar_flag",
        refusals: &[],
    },
    FlagRow {
        long: "--help",
        value_name: None,
        sample: None,
        // `clap` adds it to every sub-verb: read off the built command (`FlagRow::accepted_on`).
        applies_to: &[],
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
        short: "`-h` or `--help`, on every sub-verb: `clap` adds it to each when it builds the command, and no refusal names it. It prints that sub-verb's help page on stdout and exits 0, before the boot.",
        evidence: "crates/vike-cli/src/dispatch.rs's `run`, which answers it before the boot through crates/vike-cli/src/cmd/args.rs's `help_answer` (the `ErrorKind::DisplayHelp` arm) and `finish` (stdout, exit 0); crates/vike-utility/src/cli.rs's `Program::exit_for`, the same answer from a plane's own parse; crates/vike-cli/src/surface.rs's `FlagRow::accepted_on` reads its sub-verbs off the built command",
        refusals: &[],
    },
    FlagRow {
        long: "--html",
        value_name: None,
        sample: None,
        applies_to: &["show"],
        refusal_reaches: &[
            "ls",
            "path",
            "tag",
            "diff",
            "gate",
            "params",
            "strategies",
            "templates",
            "script-api",
            "script-check",
        ],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: None,
        shape: Some(
            "bare token — the DESTINATION is --out, exactly as it is for --export and --metrics-list, so a path on the flag would be a second spelling of the same thing (§6.2 spells --html FILE and this deviates deliberately)",
        ),
        mode_scope: "local",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "Absent = the text rendering. A bare boolean: it selects a DOCUMENT, not a value.",
        ),
        short: "`show` only: this run's whole tearsheet as one standalone HTML page, to --out or stdout. ⚠ It was a member of the unbuilt_renderers roster until the renderer's dependency edge landed, refused with a sentence blaming a missing renderer — `vike_analytics::render_html` was never missing and `Tearsheet::from_report` names show.rs by path as the caller it was built for; what was missing was the dependency. The metrics table carries report.json through value by value, but the charts, their captions and the monthly-returns table are FOLDED from series.json as stored, so on a decimated run the drawn low can read shallower than the max_drawdown printed above it. That asymmetry is why this ships where --breakdown stays refused: a picture of a sample is visibly one, while a recomputed period table would quietly disagree with report.json's own numbers.",
        evidence: "`crates/vike-cli/src/cmd/backtest/read/show.rs`'s `html_document` and `refuse_a_second_document`; its field, `crates/vike-cli/src/cmd/backtest/read.rs`'s `ReadCommand::html`.",
        refusals: &[
            Refusal {
                on: "beside any other document or section flag on `show`",
                text: Raised(FlagRefusalId::HtmlBesideASection, &[]),
            },
            Refusal { on: "--html --json", text: Raised(FlagRefusalId::HtmlBesideJson, &[]) },
            Refusal {
                on: "ls, path, tag, diff, gate, params, strategies, templates, script-api, script-check, prune",
                text: Raised(FlagRefusalId::HtmlOnlyOnShow, &[]),
            },
        ],
    },
    FlagRow {
        long: "--interval",
        value_name: Some("1h"),
        sample: Some("1h"),
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
            "`ProfileData::interval` is `#[serde(default = \"default_interval\")]` and crates/vike-backtest/src/run/profile/data_cfg.rs's default_interval returns \"1d\". The CLI supplies nothing.",
        ),
        short: "Sugar for data.interval, plus the ONE grammar check inside the match body: the trimmed value must parse through vike_marketdata::interval::interval_ms.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's RunCommand::into_args (the `s.flag == \"--interval\"` special case inside the Shape::Str arm); crates/vike-marketdata/src/interval.rs's interval_ms",
        refusals: &[Refusal {
            on: "vike_marketdata::interval::interval_ms returns None — fewer than 2 chars, a non-digit count, or an unknown trailing unit. NOTE: a ZERO count (`0m`) DOES parse and is NOT refused here.",
            text: Raised(FlagRefusalId::IntervalInvalid, &[]),
        }],
    },
    FlagRow {
        long: "--json",
        value_name: None,
        sample: None,
        // ⚠ This row read `&["run"]` and was WRONG about eight of the nine sub-verbs that accept
        // the flag, for as long as the table has existed. `run` has its own parser
        // (`parse_run_args`); the READING verbs share a second one (`parse_read`) whose
        // `"--json"` arm sets `ReadArgs::json` for every one of them — no per-sub-verb refusal
        // roster names it, and `PARAMS_REFUSED` deliberately excludes it. Two parsers, and this
        // table had only ever looked at one.
        //
        // The cost was not cosmetic. The published reference told a reader that a `gate --json`
        // pipeline named a flag that does not exist, and the documentation repository's own recipe
        // gate — which reads THIS field — would have refused the correct command line.
        // `crates/vike-cli/src/cmd/backtest/tests/reading_verbs.rs`'s
        // `the_surface_and_the_parsers_agree_about_every_flag_and_sub_verb` now drives the REAL
        // parsers over every (flag, sub-verb) pair, so no row can disagree with them again.
        // ⚠ The authoring three joined by CONSTRUCTION rather than by an edit to the parser: the
        // `"--json"` arm sits in the shared reading loop, so a new reading sub-verb accepts the
        // flag the moment it is dispatched. This row is what has to be told, which is why the gate
        // that DRIVES the parser is the only thing that can keep it honest.
        applies_to: &[
            "run",
            "ls",
            "show",
            "path",
            "tag",
            "diff",
            "gate",
            "params",
            "strategies",
            "templates",
            "script-api",
            "script-check",
            "prune",
        ],
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
        short: "Print a machine-readable document instead of the human rendering. On `run` that is the server's report JSON verbatim (and on --local it is forwarded to the spawned engine); each reading verb emits its own — `gate`'s carries the verdict, one object per criterion, and `available_keys` when a criterion went unevaluated. ⚠ ACCEPTED on `path` and read nowhere: that verb prints one line so `$(…)` works and has no document form, so the flag is inert there rather than refused. On `script-check` the document is the VERDICT (`ok`, `error`, `script`, `params`) and the rung is beside it; on `templates` it is the starter roster, one starter, or the path a starter was saved to.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `Args::json`, crates/vike-cli/src/cmd/backtest/run_args.rs's `RunCommand::json`, crates/vike-cli/src/cmd/backtest/read.rs's `ReadCommand::json`, `ReadArgs::json`, crates/vike-cli/src/cmd/backtest/execute.rs's `execute`, `execute_local` and crates/vike-cli/src/cmd/backtest/read.rs's `execute_read`; crates/vike-cli/src/cmd/backtest/read/gate.rs's `run_gate`",
        refusals: &[Refusal {
            on: "an inline value (`--json=1`)",
            text: Clap(&["run", "--json={value}"]),
        }],
    },
    FlagRow {
        long: "--kind",
        value_name: None,
        sample: Some("bar"),
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
        short: "Sugar for data.kind, plus the second in-match special case: the trimmed value goes through one_of against ReplayKind::WORDS (the data_kinds roster). It is the key implied_defaults always supplies.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's RunCommand::into_args (the `s.flag == \"--kind\"` special case calls one_of with `ReplayKind::WORDS`); crates/vike-model/src/run_request/replay.rs's `ReplayKind`; crates/vike-cli/src/cmd/backtest/run_args.rs's implied_defaults",
        refusals: &[Refusal {
            on: "a value outside data_kinds (exact, case-sensitive)",
            text: Raised(
                FlagRefusalId::ValueNotInRoster,
                &[("flag", "--kind"), ("roster", "bar|tick")],
            ),
        }],
    },
    FlagRow {
        long: "--limit",
        value_name: None,
        sample: Some("20"),
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
        evidence: "crates/vike-cli/src/cmd/backtest/read/ls.rs's `run_ls` (the `a.limit` parse + `truncate`)",
        refusals: &[
            Refusal {
                on: "ls, non-usize value",
                text: Raised(FlagRefusalId::LimitNotAWholeNumber, &[]),
            },
            Refusal {
                on: "show, path, tag, diff, gate, params, strategies, templates, script-api, script-check, prune",
                text: Raised(FlagRefusalId::OnlyOnLs, &[("flag", "--limit")]),
            },
        ],
    },
    // ⚠ **THE FIRST RETIRED SPELLING, and the row that made [`Status::Retired`] used by something.**
    // (`--store` is the second, further down, and says what it has that this one does not.)
    //
    // `--list-params` was a MODE FLAG on this plane for months; decision 11 gave it a sub-verb
    // (`backtest params`) and BOTH parse levels answer the old spelling with the new one rather
    // than with "unknown argument" — `claim_subcommand`'s arm for the bare
    // `vike-cli backtest --list-params`, and `parse_run_args`' arm for `run --list-params`.
    //
    // It was in NEITHER this table nor `cli.json` until this row, so the published reference
    // reported it as unknown while the binary answered it with a migration sentence, and the enum
    // variant built to publish exactly that fact was carried by no row at all. That is the same
    // hole class as `all_subcommands` being three sub-verbs short: the parser knows something the
    // published surface does not.
    //
    // ⚠ A FlagRow is the smaller half of what `docs/superpowers/specs/2026-09-14-cli-surface-export-design.md`
    // asks for. That design names a TOP-LEVEL `retired: [{spelling, replacement_invocation,
    // reason}]` array beside `unbuilt`, with `crate::RETIRED_COMMANDS` as the command-level twin —
    // and a new top-level key is a SHAPE change to a published document, i.e. a `SCHEMA_VERSION`
    // question and a cross-repo read (see that constant's doc). This row needs neither, and a
    // reader of `flags[]` can now see the spelling and its answer. The array stays owed.
    FlagRow {
        long: "--list-params",
        value_name: None,
        sample: None,
        // Accepted NOWHERE: the two parsers that recognise this spelling answer it with its
        // replacement. The sub-verbs its refusal REACHES are the next field, which is the split
        // `an_unbuilt_flag_accepts_nowhere_and_says_where_its_refusal_reaches` argues for —
        // ⚠ that test gates `Status::Unbuilt` only, so this row's emptiness is held by nothing but
        // the parsers themselves, which accept the spelling nowhere.
        applies_to: &[],
        // `run` is where `parse_run_args` answers it. The OTHER answer is one level up, before any
        // sub-verb is claimed at all, so no sub-verb name can express it — see the refusals below.
        refusal_reaches: &["run"],
        status: Status::Retired,
        roster_id: None,
        validated_by: "cli",
        profile_key: None,
        shape: Some("bare boolean switch — retired, so no value of any shape is accepted"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "A retired spelling has no default: it configures nothing on any route. What replaced it is a sub-verb, `vike-cli backtest params --script <s.rhai>`.",
        ),
        short: "RETIRED. A knob discovery is now `backtest params`, not a mode flag on a run — decision 11 gives every action inside this plane exactly one spelling. Both parse levels answer this spelling with its replacement.",
        evidence: "crates/vike-cli/src/refusals.rs's `ListParamsRetired`, raised by crates/vike-cli/src/cmd/backtest.rs's `Cli::list_params` and `parse` (which answers the spelling wherever it sits on the line) and crates/vike-cli/src/cmd/backtest/run_args.rs's `RunCommand::into_args` (the `self.list_params` check); crates/vike-cli/src/lib.rs's `RETIRED_COMMANDS`, the command-level twin of this doctrine",
        refusals: &[
            Refusal {
                on: "the bare plane — `vike-cli backtest --list-params`, answered by the plane's `parse` before any sub-verb is claimed (exit 2)",
                text: Raised(FlagRefusalId::ListParamsRetired, &[]),
            },
            Refusal {
                on: "run — `RunCommand::into_args` answers it with the same refusal, so the flag is answered whichever level it is typed at (exit 2)",
                text: Raised(FlagRefusalId::ListParamsRetired, &[]),
            },
        ],
    },
    FlagRow {
        long: "--local",
        value_name: None,
        sample: None,
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
            "Absent = the REMOTE arm: the profile text is shipped to a backtesthub. Present = `execute_local` spawns the standalone `backtesthub` engine on this machine instead.",
        ),
        short: "Run on THIS machine by spawning the standalone backtest engine instead of shipping the profile to a backtesthub.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `Args::local`, crates/vike-cli/src/cmd/backtest/run_args.rs's `RunCommand::local`, crates/vike-cli/src/cmd/backtest/execute.rs's `execute` and `execute_local`",
        refusals: &[
            Refusal {
                on: "--local combined with --addr (CLI, exit 2). The literal carries a real newline; the line continuations strip their own leading whitespace, so the second line begins at `drop one:`.",
                text: Raised(FlagRefusalId::AddrBesideLocal, &[]),
            },
            Refusal {
                on: "--local on a profile that declares a [walkforward] table (CLI, exit 2). One real newline before `Drop --local`.",
                text: Raised(FlagRefusalId::LocalHasNoWalkforward, &[]),
            },
            Refusal {
                on: "an inline value (`--local=1`)",
                text: Clap(&["run", "--local={value}"]),
            },
            Refusal {
                on: "--local when the profile had to be STAGED and there is no project above the working directory (exit 1). `{}` is filled by one of the two sentences below.",
                text: Raised(FlagRefusalId::NoProjectToStage, &[]),
            },
            Refusal {
                on: "--local on a profile that declares a [permutation] table (CLI, exit 2)",
                text: Raised(FlagRefusalId::LocalHasNoPermutation, &[]),
            },
        ],
    },
    FlagRow {
        long: "--markdown",
        value_name: None,
        sample: None,
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
        short: "`diff` only. Renders the same rows as a Markdown table. ⚠ `--json` WINS over it — run_diff tests `a.json` first, so `--json --markdown` emits JSON.",
        evidence: "crates/vike-cli/src/cmd/backtest/read/diff.rs's `run_diff` (the `if a.json { … } else if a.markdown { … }` ladder)",
        refusals: &[
            Refusal {
                on: "ls, show, path, tag, gate, params, strategies, templates, script-api, script-check, prune",
                text: Raised(FlagRefusalId::OnlyOnDiff, &[("flag", "--markdown")]),
            },
            Refusal {
                on: "(any reading sub-verb, with a value)",
                text: Clap(&["diff", "--markdown={value}"]),
            },
        ],
    },
    FlagRow {
        long: "--metrics",
        value_name: None,
        sample: None,
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
        evidence: "crates/vike-cli/src/cmd/backtest/read/show.rs's `show_text` and `run_show`; crates/vike-cli/src/cmd/backtest/read.rs's `refuse_foreign_read_flags` (`show_only`)",
        refusals: &[
            Refusal {
                on: "ls, path, tag, diff, gate, params, strategies, templates, script-api, script-check, prune",
                text: Raised(FlagRefusalId::OnlyOnShow, &[("flag", "--metrics")]),
            },
            Refusal {
                on: "(any reading sub-verb, with a value)",
                text: Clap(&["show", "--metrics={value}"]),
            },
            Refusal {
                on: "show, beside --metrics-list",
                text: Raised(
                    FlagRefusalId::MetricsListBesideARunRendering,
                    &[("flag", "--metrics")],
                ),
            },
        ],
    },
    FlagRow {
        long: "--metrics-list",
        value_name: None,
        sample: None,
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
            "Absent = the ordinary `show` rendering. It is NOT a section flag and does not take part in `show_text`'s `all = !metrics && !trades && !config` rule — it returns from the TOP of `run_show`, above the runs-root requirement, before any run is resolved.",
        ),
        short: "`show` only, and the ONE flag on this verb that is not about a run: it prints the METRIC CATALOG (every id with where it is stored and what it measures, plus the declared-absent rows) and stops. Takes no selector and reads no runs directory. Composes with --out; refused beside --metrics/--trades/--config/--export and beside --json.",
        evidence: "crates/vike-cli/src/cmd/backtest/read/show.rs's `run_show` (the `a.metrics_list` short-circuit), `refuse_a_listing_beside_a_run_rendering` and `run_rendering_flags_given`; crates/vike-cli/src/cmd/backtest/read.rs's `ReadCommand::metrics_list` and `ReadArgs::metrics_list`; crates/vike-analytics/src/metric_catalog.rs's `metric_list_text`, `METRICS` and `ABSENT`",
        refusals: &[
            Refusal {
                on: "ls, path, tag, diff, gate, params, strategies, templates, script-api, script-check, prune — a NAMED rule rather than a `show_only` roster row, because that roster's sentence (\"selects a SECTION of one stored run\") is FALSE of a listing",
                text: Raised(FlagRefusalId::MetricsListOnlyOnShow, &[]),
            },
            Refusal {
                on: "(any reading sub-verb, with a value)",
                text: Clap(&["show", "--metrics-list={value}"]),
            },
            Refusal {
                on: "show, beside --metrics | --trades | --config | --export (`{flag}` is whichever was given)",
                text: Raised(FlagRefusalId::MetricsListBesideARunRendering, &[]),
            },
            Refusal {
                on: "show, beside --json",
                text: Raised(FlagRefusalId::MetricsListBesideJson, &[]),
            },
        ],
    },
    FlagRow {
        long: "--note",
        value_name: None,
        sample: Some("fee model fix"),
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
        evidence: "crates/vike-cli/src/cmd/backtest/read/tag.rs's `run_tag` (passes `a.note` to both `add_tags` and `write_mark`) and that function's ⚠ doc",
        refusals: &[Refusal {
            on: "ls, show, path, diff, gate, params, strategies, templates, script-api, script-check, prune",
            text: Raised(FlagRefusalId::OnlyOnTag, &[("flag", "--note")]),
        }],
    },
    FlagRow {
        long: "--optimizer",
        value_name: Some("method"),
        sample: Some("grid"),
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: Some("optimizers"),
        validated_by: "cli",
        profile_key: None,
        shape: Some(
            "one of the optimizers roster IN ANY CASE, spelling-checked here against the protocol crate's shared flag vocabulary and RESOLVED by whichever side runs; the CANONICAL (lower-case) member is what this side forwards",
        ),
        mode_scope: "any",
        default_kind: "far_side",
        default_value: Some("grid"),
        default_conditional_on: None,
        default_reason: Some(
            "This crate substitutes NO default — an absent --optimizer is forwarded as `None` on BOTH routes and the side that RUNS resolves it against `vike_model::run_request::search_methods::DEFAULT_SEARCH_METHOD` (\"grid\"). `DEFAULT_OPTIMIZER` in vike-cli is `#[cfg(test)]`-gated and exists only to build sample argv in that file's own tests.",
        ),
        short: "The parameter-search METHOD (grid, euler, tpe, genetic). Spelling-checked locally against the protocol roster; meaning resolved by the engine or the backtesthub.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's `RunCommand::into_args` (which calls `accept_value` for `--optimizer`); crates/vike-model/src/run_request/flag_vocab.rs's `accept_value` and its `--optimizer` row, whose `values` IS the protocol const; crates/vike-cli/src/cmd/backtest.rs's `DEFAULT_OPTIMIZER` (cfg(test)); crates/vike-model/src/run_request/search_methods.rs's `SEARCH_METHODS` and `DEFAULT_SEARCH_METHOD`; crates/vike-backtest/src/search/select.rs's `resolve`. ⚠ This row cited a local `OPTIMIZERS` const and `one_of` until the client adopted the shared vocabulary; that const was already `= SEARCH_METHODS`, so its MEMBERS never drifted — its MATCH RULE did, which is the field below.",
        refusals: &[
            Refusal {
                on: "a value outside the roster, in any case (CLI, before any dial or spawn; exit 2) — the ENGINE and the backtesthub answer with this same sentence, rendered from the same roster by the same function, so there is no longer a second wording to reach",
                text: Copied("invalid --optimizer \"bogus\" (expected grid|euler|tpe|genetic)"),
            },
            Refusal {
                on: "typed on a profile that declares a [walkforward] table (CLI, exit 2)",
                text: Raised(FlagRefusalId::FlagOnWalkforward, &[("flag", "--optimizer")]),
            },
            Refusal {
                on: "--optimizer genetic with no --seed (engine/server, via search::select::require_seed)",
                text: Copied(
                    "--optimizer genetic requires --seed <u64>. It is not defaulted, deliberately: a genetic search reports ONE sample of a distribution, and a seed nobody typed is a constant the result silently depends on — write `--seed 7` (any u64) and the run is reproducible from your own shell history",
                ),
            },
            Refusal {
                on: "--local, on a profile with no [paramscan] table (the spawned engine, exit 2; `{asked}` is the space-joined subset of --optimizer --euler-depth --trials --seed --keep-trials --resume that argv actually carried, `{profile_path:?}` the Debug-quoted profile path)",
                text: Copied(
                    "backtest: {asked} names a parameter SEARCH, but {profile_path:?} has no [paramscan] table — there is nothing to search. Add one, or drop the search flags",
                ),
            },
            Refusal {
                on: "remote, on a profile with no [paramscan] table (backtesthub Response::Error; the literal contains a real newline before `fast = …`)",
                text: Copied(
                    "profile has no [paramscan] table — a parameter search needs a grid, e.g. `[paramscan]\nfast = [5, 10, 15]`",
                ),
            },
            Refusal {
                on: "any non-grid method against a daemon that does not advertise `search_method` — refused locally, NOTHING sent (`{:?}` renders the daemon's advertised feature list)",
                text: Copied(
                    "backtest daemon does not advertise `search_method` (advertised: {features:?}) — nothing was sent. A search METHOD (--optimizer/--euler-depth/--trials/--seed) and --rank-by multi need a newer `vike-backend backtesthub --addr`; this verb is capability-negotiated, not version-gated. An older daemon would DROP the field, run the exhaustive grid and report success, which is the silent downgrade this refusal exists to prevent — upgrade the daemon, or drop the flag to search the grid on the server.",
                ),
            },
            Refusal {
                on: "written with no value (trailing `--optimizer`, or `--optimizer` followed by another `--` token)",
                text: Clap(&["run", "--optimizer", "--json"]),
            },
            Refusal {
                on: "typed on a profile that declares a [permutation] table (CLI, exit 2)",
                text: Raised(FlagRefusalId::FlagOnPermutation, &[("flag", "--optimizer")]),
            },
        ],
    },
    FlagRow {
        long: "--out",
        value_name: None,
        sample: Some("runs.txt"),
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
        short: "A DOUBLE-OWNED FLAG. Owners: `ls` (writes the table or the --json document) and `show` (writes the rendered run, or the raw --export document). Refused on every other reading sub-verb by a NAMED rule, not by a roster: `gate` and `diff` are deliberately excluded because their single clean stream is what `>` already handles. The write itself is `std::fs::write(file, format!(\"{text}\\n\"))` — note the appended newline — and an existing path is OVERWRITTEN (no refusal). ⚠ That last property is why `templates` does NOT reuse this flag even though it is the one reading verb that creates a file: --write-strategy refuses an existing path, and one flag may not carry two clobber policies.",
        evidence: "crates/vike-cli/src/cmd/backtest/read.rs's `refuse_foreign_read_flags` (the `!matches!(a.sub, ReadSub::Ls | ReadSub::Show) && a.out.is_some()` rule); crates/vike-cli/src/cmd/backtest/read/ls.rs's `run_ls`; crates/vike-cli/src/cmd/backtest/read/show.rs's `run_show`",
        refusals: &[
            Refusal {
                on: "path, tag, diff, gate, params, strategies, script-api, script-check, prune",
                text: Raised(FlagRefusalId::OutNotARendering, &[]),
            },
            Refusal {
                on: "templates — the same rule with a POINTER appended, because an operator typing it there is trying to save a starter and a bare `>` would clobber the strategy they are editing",
                text: Raised(FlagRefusalId::OutOnTemplates, &[]),
            },
            Refusal {
                on: "ls or show, when the file cannot be written",
                text: Raised(FlagRefusalId::OutCannotWrite, &[]),
            },
            Refusal { on: "(any reading sub-verb, no value)", text: Clap(&["ls", "--out"]) },
        ],
    },
    FlagRow {
        long: "--param",
        value_name: Some("k=v"),
        sample: Some("size=1.5"),
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
        short: "Sugar for --set strategy.params.<k>=<v>, value typed by parse_scalar. NO schema validation is possible on either side: ProfileStrategy::params is a free-form Params bag, so a typo is a silent no-op.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's RunCommand::into_args (the `self.param` loop); crates/vike-cli/src/cmd/backtest/profile.rs's looks_like_a_search_axis",
        refusals: &[
            Refusal {
                on: "the value carries no `=`",
                text: Raised(FlagRefusalId::ParamNotKeyValue, &[]),
            },
            Refusal {
                on: "the key is empty or whitespace (refused HERE so the message names --param rather than the --set grammar)",
                text: Raised(FlagRefusalId::ParamEmptyKey, &[]),
            },
            Refusal {
                on: "the key contains a `.`",
                text: Raised(FlagRefusalId::ParamNestedKey, &[]),
            },
            Refusal {
                on: "the value looks like a search axis — it starts with `[`, or it is three colon-separated parts that all parse as f64",
                text: Raised(FlagRefusalId::ParamRange, &[]),
            },
        ],
    },
    FlagRow {
        long: "--preset",
        value_name: Some("<p.toml>"),
        sample: Some("fast.toml"),
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
        short: "A flat preset TOML merged into [strategy.params]. Given twice, it is refused.",
        evidence: "crates/vike-cli/src/cmd/backtest/profile.rs's merge_preset_params; crates/vike-cli/src/cmd/backtest/run_args.rs's RunCommand::preset",
        refusals: &[
            Refusal {
                on: "the preset wraps its knobs in a [params] table (exactly one key, and it is a table named `params`) — the message is prefixed `preset {preset_path}: ` by execute",
                text: Copied(
                    "it wraps its knobs in a [params] table, so the strategy would receive one parameter called 'params' that nothing reads and every knob would keep its default. A preset IS the params table: delete the [params] header and leave the keys at the top level",
                ),
            },
            Refusal {
                on: "the preset defines `src`",
                text: Copied(
                    "it defines 'src', which is the strategy's SOURCE rather than one of its knobs — put the strategy in a file under strategies/ and name it with `[strategy] file`. Delete the 'src' key",
                ),
            },
            Refusal {
                on: "the preset is not valid TOML",
                text: Raised(FlagRefusalId::PresetNotToml, &[]),
            },
            Refusal {
                on: "the preset is TOML but not a table",
                text: Raised(FlagRefusalId::PresetNotATable, &[]),
            },
            Refusal {
                on: "the file cannot be read",
                text: Raised(FlagRefusalId::PresetUnreadable, &[]),
            },
            Refusal {
                on: "the preset is a table whose values are not strategy parameters",
                text: Raised(FlagRefusalId::PresetNotParameters, &[]),
            },
        ],
    },
    FlagRow {
        long: "--profile",
        value_name: Some("<run.toml>"),
        sample: Some("run.toml"),
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
        short: "Optional BASE profile file; flags override it. Given twice, it is refused.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's RunCommand::profile, which RunCommand::into_args hands on as `profile_path: self.profile`; crates/vike-cli/src/cmd/backtest/execute.rs's execute reads it into `base` and hands it to build_profile_toml",
        refusals: &[
            Refusal {
                on: "a trailing --profile with nothing after it",
                text: Clap(&["run", "--profile"]),
            },
            Refusal {
                on: "the next token starts with `--`",
                text: Clap(&["run", "--profile", "--json"]),
            },
            Refusal {
                on: "the file cannot be read (in execute, exit rung 1)",
                text: Raised(FlagRefusalId::ProfileUnreadable, &[]),
            },
            Refusal {
                on: "the file is not valid TOML (in build_profile_toml, lifted onto the USAGE rung by CliError::usage)",
                text: Raised(FlagRefusalId::ProfileNotToml, &[]),
            },
            Refusal {
                on: "run, with no --profile and no flag that builds a profile (exit 2)",
                text: Raised(FlagRefusalId::NothingToRun, &[]),
            },
        ],
    },
    FlagRow {
        long: "--rank-by",
        value_name: None,
        sample: Some("sharpe"),
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: Some("rank_metrics"),
        validated_by: "cli",
        profile_key: None,
        shape: Some(
            "one of the rank_metrics roster IN ANY CASE, spelling-checked here against the protocol crate's shared flag vocabulary; the CANONICAL (lower-case) member is what this side forwards, and the metric itself is computed by whichever side runs",
        ),
        mode_scope: "any",
        default_kind: "far_side",
        default_value: Some("sharpe"),
        default_conditional_on: None,
        default_reason: Some(
            "The CLI forwards `None` when unwritten; `search::select`'s `resolve_rank` maps `None` to `RankChoice::Metric(RankMetric::default())`, and `RankMetric`'s `#[default]` variant is `Sharpe`. Deliberately NOT part of `Args::search_requested`, so it never routes a run to the search verb by itself.",
        ),
        short: "Which metric orders a parameter search's rows (sharpe, return, max_dd, equity, multi). A VALID value is IGNORED, not an error, on a profile with no grid.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `Args::rank_by`, `Args::search_requested` and crates/vike-cli/src/cmd/backtest/run_args.rs's `RunCommand::into_args` (which calls `accept_value` for `--rank-by`); crates/vike-model/src/run_request/flag_vocab.rs's `RANK_METRICS` and `accept_value`; crates/vike-backtest/src/search/select.rs's `resolve_rank`; crates/vike-backtest/src/search/paramscan/rank.rs's `RankMetric`",
        refusals: &[
            Refusal {
                on: "a value outside the roster, in any case (CLI, exit 2, before any dial or spawn) — ⚠ this row used to carry TWO messages, because the CLI refused with `--rank-by must be …, got \"bogus\"` while the engine and the server refused the same value with the sentence below; both sides render it from one roster through one function now, so the pair collapsed into this row",
                text: Copied(
                    "invalid --rank-by \"bogus\" (expected sharpe|return|max_dd|equity|multi)",
                ),
            },
            Refusal {
                on: "typed on a profile that declares a [walkforward] table (CLI, exit 2) — that wire verb carries the profile TOML and nothing else",
                text: Raised(FlagRefusalId::FlagOnWalkforward, &[("flag", "--rank-by")]),
            },
            Refusal {
                on: "--rank-by multi against a daemon that does not advertise `search_method` — refused locally, nothing sent",
                text: Copied(
                    "backtest daemon does not advertise `search_method` (advertised: {features:?}) — nothing was sent. A search METHOD (--optimizer/--euler-depth/--trials/--seed) and --rank-by multi need a newer `vike-backend backtesthub --addr`; this verb is capability-negotiated, not version-gated. An older daemon would DROP the field, run the exhaustive grid and report success, which is the silent downgrade this refusal exists to prevent — upgrade the daemon, or drop the flag to search the grid on the server.",
                ),
            },
            Refusal {
                on: "typed on a profile that declares a [permutation] table (CLI, exit 2)",
                text: Raised(FlagRefusalId::FlagOnPermutation, &[("flag", "--rank-by")]),
            },
        ],
    },
    FlagRow {
        long: "--script",
        value_name: Some("<s.rhai>"),
        sample: Some("s.rhai"),
        // ⚠ THREE owners, and the third is why this flag left `params_only`. `script-check`
        // compiles the file `params` lists the knobs of — the same file answering two questions —
        // so `refuse_foreign_read_flags` now refuses it with a NAMED rule that mentions both
        // reading owners, the way `--trades` is refused for `show` and `diff`.
        applies_to: &["params", "script-check"],
        refusal_reaches: &["run"],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: None,
        shape: None,
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "Writes nothing: on `params` and `script-check` it names a file to READ. On `run` it is no longer supported and refused, exit 4 (a strategy is a FILE under strategies/, named by the profile's [strategy] file).",
        ),
        short: "On the `params` sub-verb names the script whose knobs are listed, and on `script-check` the script to COMPILE — one meaning: a .rhai file on this machine. No longer supported on `run`: refused (exit 4), put the strategy in a file under strategies/ and name it with [strategy] file.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's `into_args` and the named `--script` rule in crates/vike-cli/src/cmd/backtest/read.rs's `refuse_foreign_read_flags`; crates/vike-cli/src/cmd/backtest/strategies.rs's `run_script_check`",
        refusals: &[
            Refusal {
                on: "run (no longer supported, exit 4)",
                text: Copied(
                    "error [backtest.script_unsupported]: --script is no longer supported: put the strategy in a file under strategies/ and name it with `[strategy] file`",
                ),
            },
            Refusal {
                on: "params, given together with --strategy",
                text: Raised(FlagRefusalId::ScriptAndStrategy, &[]),
            },
            Refusal {
                on: "params, unreadable file",
                text: Raised(FlagRefusalId::ScriptUnreadable, &[]),
            },
            Refusal {
                on: "params, script that will not compile",
                text: Raised(FlagRefusalId::ScriptDoesNotCompile, &[]),
            },
            Refusal {
                on: "script-check, given a file that does not compile (exit rung 1; the diagnostic is the PRODUCT and goes to stdout, not through this refusal)",
                text: Copied("error: {path} does not compile\\n{rhai compile error}"),
            },
            Refusal {
                on: "ls, show, path, tag, diff, gate, strategies, templates, script-api, prune",
                text: Raised(FlagRefusalId::ScriptOnlyOnParamsOrScriptCheck, &[]),
            },
            Refusal {
                on: "params, with neither --script nor --strategy",
                text: Raised(FlagRefusalId::ParamsNeedsASource, &[]),
            },
        ],
    },
    FlagRow {
        long: "--sdk",
        value_name: None,
        sample: Some("3"),
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "engine",
        profile_key: Some("strategy.sdk"),
        shape: Some("scalar (TOML-typed via parse_scalar): an SDK number, an integer"),
        mode_scope: "any",
        default_kind: "schema",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "`ProfileStrategy::sdk` is `#[serde(default)]`, so an absent key is `None`: the `.rs` file builds against its own `[strategy] sdk` pin, else the newest SDK on the box. The CLI supplies nothing.",
        ),
        short: "Sugar for strategy.sdk (decision 0296): the SDK the `.rs` strategy file is built against for THIS run, over the file's own pin, so one file can be run on two SDKs (`vike-cli strategy sdk-diff`). Forwarded unvalidated; the far side types it.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's SUGAR (row `--sdk` -> `strategy.sdk`, Shape::Scalar); crates/vike-backtest/src/run/profile.rs's `ProfileStrategy`; crates/vike-backtesthub/src/strategy_sandbox.rs's `ensure_built`",
        refusals: &[
            Refusal {
                on: "a value that is not a non-negative integer (the far side's typed read, after a dial)",
                text: Copied("invalid type: … expected u32"),
            },
            Refusal {
                on: "beside a `.rhai` strategy file (the far side, ProfileBacktest::refusals)",
                text: Copied(
                    "strategy.sdk = {sdk} names a Rust strategy SDK, and strategy.file {:?} is a .rhai file, which runs on this vike's own code: remove strategy.sdk",
                ),
            },
        ],
    },
    FlagRow {
        long: "--seed",
        value_name: Some("u64"),
        sample: Some("1"),
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
            "CONFIRMED forwarded verbatim (same three sites). `search::select`'s `parse_seed` returns `Option<u64>` and substitutes nothing; the tpe arm applies `.unwrap_or(0)` and the genetic arm calls `require_seed`, which refuses an absent seed outright.",
        ),
        short: "Reproducibility seed for the two stochastic searchers. tpe defaults it to 0; genetic REFUSES an absent one.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `Args::seed` and crates/vike-cli/src/cmd/backtest/run_args.rs's `RunCommand::seed`; crates/vike-backtest/src/search/select.rs's `parse_seed`, `require_seed` and `resolve`",
        refusals: &[
            Refusal {
                on: "--seed under grid or euler (METHOD_KNOBS row owners are [\"tpe\", \"genetic\"], joined with \" or \")",
                text: Copied(
                    "--seed is a tpe or genetic flag, but this run selected the \"grid\" optimizer. It was silently discarded before; it is refused now, because a knob that configures a search you did not select cannot do what it says",
                ),
            },
            Refusal {
                on: "a non-u64 value",
                text: Copied("invalid --seed \"abc\" (expected a u64)"),
            },
            Refusal {
                on: "--optimizer genetic with no --seed",
                text: Copied(
                    "--optimizer genetic requires --seed <u64>. It is not defaulted, deliberately: a genetic search reports ONE sample of a distribution, and a seed nobody typed is a constant the result silently depends on — write `--seed 7` (any u64) and the run is reproducible from your own shell history",
                ),
            },
            Refusal {
                on: "written with no value (`clap`, on this side, before any spawn or dial)",
                text: Clap(&["run", "--seed"]),
            },
        ],
    },
    FlagRow {
        long: "--set",
        value_name: Some("key=value"),
        sample: Some("engine.cash=10000"),
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
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's RunCommand::into_args (the `self.set` loop computes `let head = key.split('.').next().unwrap_or(\"\")` and tests it against `PROFILE_TOP_LEVEL_KEYS` less its last key, `inputs`); crates/vike-cli/src/cmd/backtest/profile.rs's set_profile_key",
        refusals: &[
            Refusal {
                on: "the value carries no `=` (split_once, first `=` only — the VALUE may contain further `=`)",
                text: Raised(FlagRefusalId::SetNotKeyValue, &[]),
            },
            Refusal {
                on: "the first dotted segment is not in PROFILE_TOP_LEVEL_KEYS — including the EMPTY head produced by a leading dot or an empty key. The trailing hole is PROFILE_TOP_LEVEL_KEYS.join(\", \").",
                text: Raised(FlagRefusalId::SetUnknownTable, &[]),
            },
            Refusal {
                on: "any segment is empty after the head check passed (e.g. `--set name.=1`) — raised by set_profile_key inside build_profile_toml, lifted onto the USAGE rung",
                text: Raised(FlagRefusalId::SetEmptySegment, &[]),
            },
            Refusal {
                on: "the path runs THROUGH a plain value; `{}` is `segs[..=i].join(\".\")`, the blocking prefix",
                text: Raised(FlagRefusalId::SetThroughAPlainValue, &[]),
            },
            Refusal {
                on: "the leaf names a whole table",
                text: Raised(FlagRefusalId::SetNamesATable, &[]),
            },
            Refusal {
                on: "the profile root is not a TOML table",
                text: Raised(FlagRefusalId::ProfileRootNotATable, &[]),
            },
        ],
    },
    FlagRow {
        long: "--show-effective",
        value_name: None,
        sample: None,
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
        evidence: "crates/vike-cli/src/cmd/backtest/execute.rs's execute (`if args.show_effective { print!(...); return Ok(()); }`); crates/vike-cli/src/cmd/backtest/profile.rs's render_effective",
        refusals: &[Refusal {
            on: "an inline value (`--show-effective=1`)",
            text: Clap(&["run", "--show-effective={value}"]),
        }],
    },
    FlagRow {
        long: "--slippage",
        value_name: None,
        sample: Some("0.0005"),
        // Accepted NOWHERE since the owner's ruling of 2026-11-18, as `--fee`.
        applies_to: &[],
        refusal_reaches: &["run"],
        status: Status::Retired,
        roster_id: None,
        validated_by: "cli",
        profile_key: None,
        shape: Some("retired, so no value of any shape is accepted"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "A retired spelling has no default: it configures nothing on any route. What replaced it is the strategy's `slippage` param (`--param slippage=<amount>`), whose default is the strategy's own slippage, else none.",
        ),
        short: "RETIRED. A run's slippage is a strategy parameter now (the owner's ruling of 2026-11-18): --param slippage=<amount> with slippage_unit (and slippage_tick for ticks), or --set strategy.params.slippage. The run-level [engine.slippage] it wrote is refused by the profile loader too.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's `SLIPPAGE_RETIRED` (answered in `RunCommand::into_args`); crates/vike-backtest/src/run/costs.rs's `refuse_run_level_costs`",
        refusals: &[Refusal {
            on: "run (retired)",
            text: Raised(FlagRefusalId::SlippageRetired, &[("units", "percent|ticks")]),
        }],
    },
    FlagRow {
        long: "--sort",
        value_name: None,
        sample: Some("sharpe:desc"),
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
        evidence: "crates/vike-cli/src/cmd/backtest/read/ls.rs's `sort_rows`",
        refusals: &[
            Refusal {
                on: "ls, with a direction suffix that is not asc/desc",
                text: Raised(FlagRefusalId::SortDirection, &[]),
            },
            Refusal {
                on: "ls, when the store is non-empty and NO run carries the field",
                text: Raised(FlagRefusalId::SortNoRunCarriesTheField, &[]),
            },
            Refusal {
                on: "show, path, tag, diff, gate, params, strategies, templates, script-api, script-check, prune",
                text: Raised(FlagRefusalId::OnlyOnLs, &[("flag", "--sort")]),
            },
        ],
    },
    // ⚠ **THE SECOND RETIRED SPELLING, and unlike `--list-params` it has no new spelling on this
    // plane.** `--store DIR` named the hist-store root the `--local` engine read, and was
    // `Status::Ships` with `mode_scope: "local"`, until the owner closed the local READ door on
    // 2026-09-25: every history read goes through a datahub, so the directory is served by a
    // datahub started beside the run, and the refusal names that command rather than a flag.
    FlagRow {
        long: "--store",
        value_name: Some("DIR"),
        sample: Some("/data"),
        // Accepted NOWHERE: `parse_run_args` answers it on both arms, before `--local` is consulted
        // and before its value is read. `params` no longer lists it on `PARAMS_REFUSED` — that
        // roster's sentence says the flag "belongs to `vike-cli backtest run`", and it does not.
        applies_to: &[],
        refusal_reaches: &["run"],
        status: Status::Retired,
        roster_id: None,
        validated_by: "cli",
        profile_key: None,
        shape: Some("a directory path — retired, so it is refused whatever it names"),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "A retired spelling has no default: it configures nothing on any route. History is read from the datahub the `config.datahub_addr` row names (loopback by default); for local files, start one on them first — `vike-backend datahub --store DIR`, whose access list must hold this box's device key.",
        ),
        short: "RETIRED on 2026-09-25. Every history read goes through a datahub (decision 0084), so a run over files on this machine reads them through a loopback datahub started on them, which admits this box's device key from its access list. Refused on both arms with that command named — never an unknown argument, because somebody who typed it believes a directory will be read.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's `RunCommand::into_args` (the `self.store` check) and crates/vike-cli/src/cmd/backtest/tests/route_and_spine.rs's `the_store_flag_is_refused_on_both_arms_with_its_replacement`; crates/vike-model/src/run_request/flag_vocab.rs's `store_flag_removed`, the one sentence every history reader prints",
        refusals: &[Refusal {
            on: "run, either arm — answered before `--local` is consulted and before a value is read (exit 2)",
            text: Copied(
                "backtest run: `--store DIR` no longer reads history locally — every history READ goes through a datahub since 2026-09-25 (decision 0084). For a local run, start one beside it with `vike-backend datahub --store DIR`: it binds loopback only, and this verb dials loopback by default. Like every datahub it admits only the device keys on its project's access list (`vike-cli backend mint-missing` registers this box's own).",
            ),
        }],
    },
    FlagRow {
        long: "--strategy",
        value_name: None,
        sample: Some("trend/buy_hold.rs"),
        applies_to: &["run", "params"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "none",
        profile_key: Some("strategy.file"),
        shape: Some("Str"),
        mode_scope: "any",
        default_kind: "far_side",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "The CLI supplies nothing when unwritten and ProfileStrategy::file carries no serde default, so a profile with no [strategy] file is refused on the far side.",
        ),
        short: "Sugar for strategy.file: the strategy file's path under the backtesthub's strategies/ folder, forwarded UNVALIDATED (the folder is the server's, which this crate cannot see). On the `params` sub-verb the same spelling names the file of this project's strategies/ folder whose declared keys are listed.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's SUGAR (row `--strategy` -> `strategy.file`); crates/vike-cli/src/cmd/backtest/params.rs's `header_of`",
        refusals: &[
            Refusal {
                on: "params, given together with --script",
                text: Raised(FlagRefusalId::ScriptAndStrategy, &[]),
            },
            Refusal {
                on: "params, with no project above the working directory",
                text: Raised(FlagRefusalId::StrategyWithoutProject, &[]),
            },
            Refusal {
                on: "params, a file that cannot be read or whose header does not parse",
                text: Raised(FlagRefusalId::StrategyUnreadable, &[]),
            },
            Refusal {
                on: "ls, show, path, tag, diff, gate, strategies, templates, script-api, script-check, prune",
                text: Raised(FlagRefusalId::OnlyOnParams, &[("flag", "--strategy")]),
            },
        ],
    },
    FlagRow {
        long: "--symbol",
        value_name: None,
        sample: Some("BTCUSDT,ETHUSDT"),
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
            "`ProfileData::symbols` is `#[serde(default)] Vec<String>` in crates/vike-backtest/src/run/profile/data_cfg.rs; empty is the schema default and the CLI supplies nothing.",
        ),
        short: "The ONE row that does not push per occurrence: every occurrence comma-splits and accumulates, and a SINGLE data.symbols array Override is pushed after the loop, in argv order.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's RunCommand::into_args (the SUGAR loop's `s.flag == \"--symbol\"` branch accumulates into `symbols`; the post-loop block pushes one Override with Origin::Sugar(\"--symbol\")); pinned by crates/vike-cli/src/cmd/backtest/tests/builder_sugar_param.rs's symbol_is_repeatable_and_comma_splitting_and_yields_one_override",
        refusals: &[Refusal {
            on: "any empty element after trimming — a leading, trailing or doubled comma, or an empty value. The flag name in the message is HARD-CODED `--symbol` even though the arm is generic over Shape::StrList (it is the only StrList row).",
            text: Raised(FlagRefusalId::SymbolEmptyElement, &[]),
        }],
    },
    FlagRow {
        long: "--to",
        value_name: Some("DATE"),
        sample: Some("2026-02-01T00"),
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
            "`ProfileData::to` carries NO serde default in crates/vike-backtest/src/run/profile/data_cfg.rs. The CLI implies nothing.",
        ),
        short: "Sugar for data.to; same Shape::Str string-preservation rule as --from. No CLI-side date validation of any kind.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's SUGAR (row `--to` -> `data.to`, Shape::Str)",
        refusals: &[],
    },
    FlagRow {
        long: "--trades",
        value_name: None,
        sample: None,
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
        evidence: "crates/vike-cli/src/cmd/backtest/read.rs's `refuse_foreign_read_flags` (the `!matches!(a.sub, ReadSub::Show | ReadSub::Diff) && a.trades` rule, and the ⚠ comment above `show_only` saying why it is not in that array); crates/vike-cli/src/cmd/backtest/read/show.rs's `show_text`; crates/vike-cli/src/cmd/backtest/read/diff.rs's `run_diff`",
        refusals: &[
            Refusal {
                on: "ls, path, tag, gate, params, strategies, templates, script-api, script-check, prune",
                text: Raised(FlagRefusalId::TradesOnlyOnShowOrDiff, &[]),
            },
            Refusal {
                on: "(any reading sub-verb, with a value)",
                text: Clap(&["show", "--trades={value}"]),
            },
        ],
    },
    FlagRow {
        long: "--trials",
        value_name: None,
        sample: Some("8"),
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
            "CONFIRMED forwarded verbatim: `parse_run_args` stores the raw String, `execute_local` pushes `(\"--trials\", &args.trials)` into the child argv unchanged, and `Args::wire_search` clones it into `SearchSelectionWire.trials` as a String. The default is applied only when tpe is the resolved method, by `search::select`'s `parse_trials` → `TpeConfig::DEFAULT_TRIALS` = 64.",
        ),
        short: "tpe's trial budget. Forwarded as the token you typed; the engine or the server parses and refuses it.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `Args::trials` and crates/vike-cli/src/cmd/backtest/run_args.rs's `RunCommand::trials`; crates/vike-cli/src/cmd/backtest/execute.rs's `execute_local`; crates/vike-backtest/src/search/select.rs's `parse_trials`; crates/vike-model/src/run_request/method_knobs.rs's `METHOD_KNOBS`; crates/vike-ml-training/src/search/tpe.rs's `DEFAULT_TRIALS`",
        refusals: &[
            Refusal {
                on: "⚠ --trials under --optimizer euler (and under grid, i.e. under no --optimizer at all) — refused from `METHOD_KNOBS` by `refuse_unowned_knob`, on BOTH routes. `{method:?}` is the Debug of the method TOKEN, so it renders quoted.",
                text: Copied(
                    "--trials is a tpe flag, but this run selected the \"euler\" optimizer. It was silently discarded before; it is refused now, because a knob that configures a search you did not select cannot do what it says",
                ),
            },
            Refusal {
                on: "--trials with no --optimizer written at all (name falls back to DEFAULT_SEARCH_METHOD)",
                text: Copied(
                    "--trials is a tpe flag, but this run selected the \"grid\" optimizer. It was silently discarded before; it is refused now, because a knob that configures a search you did not select cannot do what it says",
                ),
            },
            Refusal {
                on: "a value that is not a positive integer, under tpe (engine/server)",
                text: Copied("invalid --trials \"abc\" (expected a positive integer)"),
            },
            Refusal {
                on: "written with no value (`clap`, on this side, before any spawn or dial)",
                text: Clap(&["run", "--trials"]),
            },
            Refusal {
                on: "against a daemon that does not advertise `search_method` — refused locally, nothing sent",
                text: Copied(
                    "backtest daemon does not advertise `search_method` (advertised: {features:?}) — nothing was sent. A search METHOD (--optimizer/--euler-depth/--trials/--seed) and --rank-by multi need a newer `vike-backend backtesthub --addr`; this verb is capability-negotiated, not version-gated. An older daemon would DROP the field, run the exhaustive grid and report success, which is the silent downgrade this refusal exists to prevent — upgrade the daemon, or drop the flag to search the grid on the server.",
                ),
            },
        ],
    },
    FlagRow {
        long: "--venue",
        value_name: None,
        sample: Some("binance"),
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
            "`ProfileData::venue` is `#[serde(default)] Option<String>` in crates/vike-backtest/src/run/profile/data_cfg.rs, so an absent venue is None (the cross-venue [[data.series]] form is the alternative). The CLI supplies nothing.",
        ),
        short: "Sugar for data.venue, value trimmed and written as a TOML string. No roster check here — a venue name is forwarded unvalidated.",
        evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's SUGAR (row `--venue` -> `data.venue`, Shape::Str); crates/vike-backtest/src/run/profile/data_cfg.rs's ProfileData",
        refusals: &[Refusal {
            on: "a missing or flag-shaped value",
            text: Clap(&["run", "--venue"]),
        }],
    },
    FlagRow {
        long: "--version",
        value_name: None,
        sample: None,
        // `clap` adds it to every sub-verb (`propagate_version`): read off the built command.
        applies_to: &[],
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
        short: "`-V` or `--version`, on every sub-verb: the shared `clap` settings give each one (`propagate_version`), and no refusal names it. It prints the program's one version line, `vike-cli <version> (<build identity>)`, on stdout and exits 0, before the boot.",
        evidence: "crates/vike-utility/src/cli.rs's `Program::command` (`.version(PLACEHOLDER_VERSION)` and `.propagate_version(true)`) and `Program::exit_for` (the `ErrorKind::DisplayVersion` arm); crates/vike-cli/src/dispatch.rs's `run`, which answers it before the boot through crates/vike-cli/src/cmd/args.rs's `help_answer` (the `ErrorKind::DisplayVersion` arm prints `version_line`) and `finish` (stdout, exit 0); crates/vike-cli/src/surface.rs's `FlagRow::accepted_on` reads its sub-verbs off the built command",
        refusals: &[],
    },
    FlagRow {
        long: "--where",
        value_name: None,
        sample: Some("kind=backtest,sharpe>1"),
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
        evidence: "crates/vike-cli/src/cmd/backtest/read.rs's `refuse_foreign_read_flags` (`ls_only`); crates/vike-cli/src/cmd/backtest/read/where_expr.rs's `OPS` and `filter`",
        refusals: &[
            Refusal {
                on: "the expression contains a '|' anywhere ({expr} = the whole raw expression, untrimmed)",
                text: Raised(FlagRefusalId::WhereHasNoOr, &[]),
            },
            Refusal {
                on: "a term between two commas is empty ({expr} = the whole raw expression; OP_HELP is the literal '>= | <= | != | ~ | = | > | <')",
                text: Raised(
                    FlagRefusalId::WhereEmptyTerm,
                    &[("OP_HELP", crate::cmd::backtest::read::where_expr::OP_HELP)],
                ),
            },
            Refusal {
                on: "a term contains none of the seven operators ({term} = the TRIMMED term, not the whole expression)",
                text: Raised(
                    FlagRefusalId::WhereNoOperator,
                    &[("OP_HELP", crate::cmd::backtest::read::where_expr::OP_HELP)],
                ),
            },
            Refusal {
                on: "the text before the operator is empty, e.g. '>1' ({term} = the trimmed term, {op} = the matched operator)",
                text: Raised(
                    FlagRefusalId::WhereNoField,
                    &[("OP_HELP", crate::cmd::backtest::read::where_expr::OP_HELP)],
                ),
            },
        ],
    },
    FlagRow {
        long: "--whole",
        value_name: None,
        sample: None,
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
            "A bare boolean; it selects which answer a single backtest or a parameter search gives. Absent = the SUMMARY (decision 0078's amendments of 2026-11-18 and 2026-11-19): a single run's every metric and its equity curve thinned, a search's points as their overrides and metric tables; the trades and the whole curve stay on the backtesthub.",
        ),
        short: "A single backtest or a parameter search answers its WHOLE report (every closed trade, the forecasts) instead of its summary. The default changed on 2026-11-18, a search's on 2026-11-19: a script that reads trades from the output must pass it. A search falls back to the whole report from a backtesthub without the summary. A walk-forward, a permutation test, a data plan and --local answer their reports either way.",
        evidence: "crates/vike-cli/src/cmd/backtest.rs's `Args::whole`; crates/vike-cli/src/cmd/backtest/execute.rs's execute (the `Route::Single` summary branch, `run_backtest_summary`, above the carrier match, and the `Route::Search` arm, `run_paramscan_profile_answer`)",
        refusals: &[
            Refusal {
                on: "an inline value (`--whole=1`)",
                text: Clap(&["run", "--whole={value}"]),
            },
            Refusal {
                on: "without it, a backtesthub that does not advertise `run_curve` (exit rung 1, nothing sent)",
                text: Copied(
                    "this backtesthub does not advertise `run_curve` (advertised: {features}) — nothing was sent. It is older than this client, which asks for a backtest's summary and reads its whole equity curve in pages: deploy the backtesthub of this client's release, or ask for the whole answer (`vike-cli backtest run --whole`).",
                ),
            },
        ],
    },
    FlagRow {
        long: "--write-profile",
        value_name: Some("<out.toml>"),
        sample: Some("out.toml"),
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
        short: "Writes the fully-resolved profile (after the --preset rewrite) and then RUNS. Refuses an existing path outright. Given twice, it is refused.",
        evidence: "crates/vike-cli/src/cmd/backtest/execute.rs's execute (the `args.write_profile` block sits BELOW both client-side rewrites and above the show_effective return)",
        refusals: &[
            Refusal {
                on: "the path already exists (USAGE rung, exit 2)",
                text: Raised(FlagRefusalId::WriteProfileExists, &[]),
            },
            Refusal {
                on: "the write fails (exit rung 1)",
                text: Raised(FlagRefusalId::WriteProfileFailed, &[]),
            },
        ],
    },
    FlagRow {
        long: "--write-strategy",
        value_name: Some("<s.rhai>"),
        sample: Some("s.rhai"),
        applies_to: &["templates"],
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
        default_reason: Some(
            "Absent = the starter's source goes to stdout, which is what makes `templates <id> > s.rhai` work. Present = it is written to that path and the PATH is what stdout carries, so `$(…)` is usable.",
        ),
        short: "`templates` only. Saves the named starter to a file that does NOT exist yet — a `--write-profile`-shaped rule, and the reason this is not `--out`: that flag overwrites, and the file this one writes is a strategy somebody then edits, so clobbering it from a flag typo is unrecoverable. It needs a starter id: given alone it is a usage refusal rather than a roster listing, because a listing plus exit 0 reads exactly like a write that happened.",
        evidence: "crates/vike-cli/src/cmd/backtest/strategies.rs's `run_templates`; crates/vike-cli/src/cmd/backtest/read.rs's `ReadArgs::write_strategy` and the `templates_only` roster in `refuse_foreign_read_flags`",
        refusals: &[
            Refusal {
                on: "the path already exists (USAGE rung, exit 2)",
                text: Raised(FlagRefusalId::WriteStrategyExists, &[]),
            },
            Refusal {
                on: "given with no starter id (USAGE rung, exit 2; `{roster}` is the derived id list)",
                text: Raised(FlagRefusalId::WriteStrategyNeedsAStarter, &[]),
            },
            Refusal {
                on: "the write fails (exit rung 1)",
                text: Raised(FlagRefusalId::WriteStrategyFailed, &[]),
            },
            Refusal {
                on: "ls, show, path, tag, diff, gate, params, strategies, script-api, script-check, prune",
                text: Raised(FlagRefusalId::OnlyOnTemplates, &[("flag", "--write-strategy")]),
            },
            Refusal {
                on: "run — a DIFFERENT answer, because `run` has its own parser and this token is not in it",
                text: Clap(&["run", "--write-strategy", "s.rhai"]),
            },
            Refusal { on: "written with no value", text: Clap(&["templates", "--write-strategy"]) },
        ],
    },
    // ── the data plane's five: a plan-then-apply door, a coverage gate and a universe mode ──────
    FlagRow {
        long: "--explain-data",
        value_name: None,
        sample: None,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: Some("data.explain"),
        shape: None,
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "A bare boolean: it selects a print-and-stop mode rather than a value.",
        ),
        short: "Print exactly what the run WILL read — every series, the resolved range, the store root and the rung that chose it, with row counts — and stop without computing.",
        evidence: "crates/vike-backtest/src/run/data_plan/verdict.rs's `plan_data` and `explain_lines`; crates/vike-cli/src/cmd/data.rs's `explain_data_override`",
        refusals: &[Refusal {
            on: "an inline value (`--explain-data=1`)",
            text: Clap(&["run", "--explain-data={value}"]),
        }],
    },
    FlagRow {
        long: "--require-coverage",
        value_name: None,
        sample: None,
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: Some("data.require_coverage"),
        shape: None,
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "A bare boolean that ARMS the gate. It says nothing about the disposition — absent `--on-gap` means refuse, which is the strict reading a gate should default to.",
        ),
        short: "Refuse a run whose window the store does not cover, naming the missing spans per series. The failure it ends: a window with a complete trade tape and no book runs to completion and REPORTS FILLS.",
        evidence: "crates/vike-backtest/src/run/data_plan/verdict.rs's `enforce` and `coverage_verdict`; crates/vike-data/src/store/coverage.rs's `window_shortfall`",
        refusals: &[Refusal {
            on: "an inline value (`--require-coverage=1`)",
            text: Clap(&["run", "--require-coverage={value}"]),
        }],
    },
    FlagRow {
        long: "--max-gap",
        value_name: None,
        sample: Some("1d"),
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: None,
        validated_by: "cli",
        profile_key: Some("data.max_gap"),
        shape: Some(
            "a duration (`1d`, `4h`) or a plain millisecond count; the grammar itself is judged by whichever side runs",
        ),
        mode_scope: "any",
        default_kind: "none",
        default_value: None,
        default_conditional_on: None,
        default_reason: Some(
            "Unwritten, every interior hole counts against the gate. A tolerance is a deliberate act, so there is no default one.",
        ),
        short: "How large an interior hole `--require-coverage` tolerates. A PER-SPAN tolerance, not a total: a shortfall that is the whole series ignores it under every value.",
        evidence: "crates/vike-backtest/src/run/data_plan.rs's `CoverageGate`; crates/vike-cli/src/cmd/data.rs's `max_gap_override`",
        refusals: &[Refusal { on: "a blank value", text: Raised(FlagRefusalId::MaxGapBlank, &[]) }],
    },
    FlagRow {
        long: "--on-gap",
        value_name: Some("DISPOSITION"),
        sample: Some("warn"),
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: Some("gap_dispositions"),
        validated_by: "cli",
        profile_key: Some("data.on_gap"),
        shape: None,
        mode_scope: "any",
        default_kind: "implied",
        default_value: Some("refuse"),
        default_conditional_on: Some("--require-coverage"),
        default_reason: Some(
            "An armed gate with no stated disposition refuses. The other way round — arming a gate that then runs anyway — is the shape that reads as checked while checking nothing.",
        ),
        short: "What an armed coverage gate DOES about an uncovered window: refuse stops the run, warn logs the findings and runs unchanged, run leaves the verdict in the document only.",
        evidence: "crates/vike-backtest/src/run/data_plan/verdict.rs's `CoverageVerdict`; crates/vike-backtest/src/run/profile/data_cfg.rs's `ProfileData::on_gap`",
        refusals: &[Refusal {
            on: "a value outside the roster",
            text: Raised(FlagRefusalId::OnGapNotAMember, &[("roster", "refuse | warn | run")]),
        }],
    },
    FlagRow {
        long: "--universe",
        value_name: Some("MODE"),
        sample: Some("strict"),
        applies_to: &["run"],
        refusal_reaches: &[],
        status: Status::Ships,
        roster_id: Some("universe_modes"),
        validated_by: "cli",
        profile_key: Some("data.universe"),
        shape: None,
        mode_scope: "any",
        default_kind: "implied",
        default_value: Some("declared"),
        default_conditional_on: None,
        default_reason: Some(
            "Unwritten, the symbol list is taken verbatim — byte-identical to every profile written before this key existed.",
        ),
        short: "What to do about a member whose tape does not span the window: declared takes the list verbatim, covered names the short members and runs anyway, strict refuses. NONE of the three drops a member.",
        evidence: "crates/vike-backtest/src/run/data_plan/verdict.rs's `universe_verdict` and crates/vike-model/src/run_request/gap.rs's `UniverseMode`; crates/vike-backtest/src/run/profile/data_cfg.rs's `ProfileData::universe`",
        refusals: &[Refusal {
            on: "a value outside the roster",
            text: Raised(
                FlagRefusalId::UniverseNotAMember,
                &[("roster", "declared | covered | strict")],
            ),
        }],
    },
];

/// The verb names of `crate::COMMANDS`, in its order: the `top_level_commands` roster, derived at
/// compile time so the published list cannot fall short of what `--help` lists.
const TOP_LEVEL_COMMAND_WORDS: [&str; crate::COMMANDS.len()] = {
    let mut out = [""; crate::COMMANDS.len()];
    let mut i = 0;
    while i < out.len() {
        out[i] = crate::COMMANDS[i].0;
        i += 1;
    }
    out
};

/// The reading sub-verbs' words, in `READ_SUBCOMMANDS`' order: the `read_subcommands` roster.
const READ_SUBCOMMAND_WORDS: [&str; crate::cmd::backtest::read::READ_SUBCOMMANDS.len()] = {
    let subs = crate::cmd::backtest::read::READ_SUBCOMMANDS;
    let mut out = [""; crate::cmd::backtest::read::READ_SUBCOMMANDS.len()];
    let mut i = 0;
    while i < out.len() {
        out[i] = subs[i].as_str();
        i += 1;
    }
    out
};

/// Text rendered once, when [`ROSTERS`] is first read, and kept for the process like the `static`
/// that holds it: a row's members are `'static` slices, and a few come from the `clap` tree or a
/// table rendered as `word -> key`, which no `const` can build.
fn kept<S: Into<String>>(rows: impl IntoIterator<Item = S>) -> &'static [&'static str] {
    rows.into_iter().map(|row| &*String::leak(row.into())).collect::<Vec<_>>().leak()
}

/// Every value roster a flag is checked against. Built on first use, because some members are read
/// off the `clap` tree (`sub_verbs`) or rendered from the table that owns them; every member list
/// here reads its source but `failif_metrics` (no roster exists) and `selector_doc_table_forms`
/// (prose, held to its doc by `surface_tests.rs`'s
/// `the_selector_doc_table_roster_is_the_module_doc_table`).
pub static ROSTERS: LazyLock<[RosterRow; 26]> = LazyLock::new(|| {
    [
        RosterRow {
            id: "all_subcommands",
            members: kept(sub_verbs()),
            refused_members: &[],
            match_rule: "exact",
            derived_from: Some("crates/vike-cli/src/surface.rs's sub_verbs"),
            admission: None,
            evidence: "crates/vike-cli/src/surface.rs's `sub_verbs` — the plane's `clap` command (crates/vike-cli/src/cmd/backtest.rs's `command`), read off its subcommands in declaration order; the same list this document's `sub_verbs` field publishes, and this row's `members` IS that read.",
        },
        RosterRow {
            id: "data_kinds",
            members: &vike_model::run_request::replay::ReplayKind::WORDS,
            refused_members: &[],
            match_rule: "exact",
            derived_from: Some("vike_model::run_request::replay::ReplayKind::WORDS"),
            admission: None,
            evidence: "crates/vike-model/src/run_request/replay.rs's `ReplayKind` (its words are the profile's serde tags), checked by crates/vike-cli/src/cmd/backtest/run_args.rs's one_of — a SPELLING check, never a second implementation of what the value MEANS. The comparison is `roster.contains(&value)` on the already-trimmed string, so it is case-SENSITIVE: the value is written verbatim into the profile TOML, whose deserializer is exact.",
        },
        RosterRow {
            id: "decide_modes",
            members: &vike_model::run_request::decide::DecideMode::WORDS,
            refused_members: &[],
            match_rule: "ascii_ci",
            derived_from: Some("vike_model::run_request::decide::DecideMode::WORDS"),
            admission: None,
            evidence: "crates/vike-model/src/run_request/decide.rs's `DecideMode`, checked by crates/vike-cli/src/cmd/backtest/run_args.rs's RunCommand::into_args through `DecideMode::parse` (ASCII case folded — hence `ascii_ci`, the rule crates/vike-backtest/src/run/profile.rs's `decide_mode` resolves the forwarded value by)",
        },
        RosterRow {
            id: "exit_ladder",
            members: kept(crate::exit::Exit::ALL.map(|rung| format!("{} {rung:?}", rung as u8))),
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
            // The model's table, its rows whose word is not its key, in its order.
            members: kept(
                vike_model::run_request::rank_report::RANK_METRIC_REPORTS
                    .iter()
                    .filter(|r| r.metric != r.report_key)
                    .map(|r| format!("{} -> {}", r.metric, r.report_key)),
            ),
            refused_members: &[],
            match_rule: "exact",
            derived_from: Some("vike_model::run_request::rank_report::RANK_METRIC_REPORTS"),
            admission: None,
            evidence: "crates/vike-model/src/run_request/rank_report.rs's `RANK_METRIC_REPORTS` (each rank word with the report key it is under; the rows whose word IS its key, `sharpe`, alias nothing), read by crates/vike-cli/src/cmd/backtest/read/failif.rs's parse_one through `rank_metric_report` — an EXACT, case-SENSITIVE, whole-string match, falling back to the metric as typed.",
        },
        RosterRow {
            id: "failif_metrics",
            members: &[],
            refused_members: &[],
            match_rule: "open_tail",
            derived_from: None,
            admission: Some(
                "OPEN — there is NO metric roster. parse_one resolves a metric against the rank-word aliases (`vike_model::run_request::rank_report::RANK_METRIC_REPORTS`) and otherwise keeps the spelling verbatim as the JSON key, so ANY key of report.json (and any key it gains later) is accepted. An unknown metric is NOT a parse error.",
            ),
            evidence: "crates/vike-cli/src/cmd/backtest/read/failif.rs's parse_one (`.map_or_else(|| metric.to_string(), ...)`) and the module doc's fourth rule, 'A metric is resolved against the DOCUMENT, not a roster'; crates/vike-cli/src/cmd/backtest/read/failif.rs's judge returns Outcome::Unevaluated when either side carries no finite number under the key.",
        },
        RosterRow {
            id: "ls_default_cols",
            members: &crate::cmd::backtest::read::ls::DEFAULT_COLS,
            refused_members: &[],
            match_rule: "exact",
            derived_from: Some("crates/vike-cli/src/cmd/backtest/read/ls.rs's DEFAULT_COLS"),
            admission: None,
            evidence: "crates/vike-cli/src/cmd/backtest/read/ls.rs's `DEFAULT_COLS`, used by `run_ls` when `--cols` is absent",
        },
        RosterRow {
            id: "ls_field_vocabulary",
            // The names `field_of` resolves by name, then its two fallthroughs.
            members: kept(
                crate::cmd::backtest::read::ls::NAMED_FIELDS.iter().map(|(name, _)| *name).chain([
                    "detail.<dotted.path>",
                    "<any other name> = a TOP-LEVEL key of report.json",
                ]),
            ),
            refused_members: &[],
            match_rule: "open_tail",
            derived_from: Some("crates/vike-cli/src/cmd/backtest/read/ls.rs's field_of"),
            admission: None,
            evidence: "crates/vike-cli/src/cmd/backtest/read/ls.rs's `field_of` — the ONE resolver `--cols`, `--sort` and `--where` all read through. The last two rows are FALLTHROUGHS, not enumerated names, so this roster has no closed membership.",
        },
        RosterRow {
            id: "method_knobs",
            members: kept(
                vike_model::run_request::method_knobs::METHOD_KNOBS
                    .iter()
                    .map(|(flag, owners)| format!("{flag} => {}", owners.join(", "))),
            ),
            refused_members: &[],
            match_rule: "ascii_ci",
            derived_from: Some("vike_model::run_request::method_knobs::METHOD_KNOBS"),
            admission: None,
            evidence: "crates/vike-model/src/run_request/method_knobs.rs's `METHOD_KNOBS` (`pub const METHOD_KNOBS: &[(&str, &[&str])]`); crates/vike-backtest/src/search/select.rs's `SearchSelection::knob`, `resolve` and `refuse_unowned_knob`; crates/vike-backtesthub/src/backtest_cli/search_flags.rs's `parse_search_flags`, which iterates the SAME table on argv PRESENCE before any value is parsed",
        },
        RosterRow {
            id: "optimizers",
            members: &vike_model::run_request::search_methods::SEARCH_METHODS,
            refused_members: &[],
            match_rule: "ascii_ci",
            derived_from: Some("vike_model::run_request::search_methods::SEARCH_METHODS"),
            admission: None,
            evidence: "crates/vike-model/src/run_request/search_methods.rs's `SEARCH_METHODS`, reached by the CLI through crates/vike-model/src/run_request/flag_vocab.rs's `--optimizer` row (whose `values` IS that const, held so by its own `the_optimizer_roster_is_the_protocol_const`) and `accept_value`; crates/vike-cli/tests/shipped_cli/backtest_cli/local.rs's `every_roster_method_is_accepted_on_both_routes`, which iterates the protocol const over both routes. ⚠ `match` was `exact` here and that was the CLI's bug rather than this table's: `--optimizer TPE` resolved on the engine (`eq_ignore_ascii_case`) and was a local exit-2, because the client owned a second match rule over the same members. It owns neither now — crates/vike-cli/src/cmd/backtest/tests/route_and_spine.rs's `an_upper_case_selector_is_accepted_and_canonicalised_by_the_shared_vocabulary` drives the real parser over the upper-case spelling.",
        },
        RosterRow {
            id: "params_refused",
            members: crate::cmd::backtest::run_args::PARAMS_REFUSED,
            refused_members: kept(crate::cmd::backtest::run_args::PARAMS_REFUSED.iter().map(
                |flag| {
                    format!(
                        "{flag} :: {}",
                        crate::cmd::backtest::run_args::refuse_a_run_flag_on_params(flag)
                    )
                },
            )),
            match_rule: "exact",
            derived_from: Some("crates/vike-cli/src/cmd/backtest/run_args.rs's PARAMS_REFUSED"),
            admission: None,
            evidence: "crates/vike-cli/src/cmd/backtest/run_args.rs's `PARAMS_REFUSED` (this row's `members` IS that const) and `refuse_a_run_flag_on_params` (each refused member's sentence), consumed by crates/vike-cli/src/cmd/backtest/read.rs's `ReadCommand::into_args` (`sub == ReadSub::Params && PARAMS_REFUSED.contains(flag)`)",
        },
        RosterRow {
            id: "path_file_sugar",
            members: kept(
                crate::cmd::backtest::read::path::FILE_SUGAR
                    .iter()
                    .flat_map(|(sugar, file)| [*sugar, *file]),
            ),
            refused_members: &[],
            match_rule: "open_tail",
            derived_from: Some("crates/vike-cli/src/cmd/backtest/read/path.rs's resolve_file"),
            admission: None,
            evidence: "crates/vike-cli/src/cmd/backtest/read/path.rs's `resolve_file` — `manifest`/`manifest.json` → MANIFEST_FILE (\"manifest.json\"), `report`/`report.json` → REPORT_FILE (\"report.json\"); anything else joins VERBATIM, so this is sugar over an open set",
        },
        RosterRow {
            id: "profile_top_level_keys",
            // The model's keys less its LAST, `inputs`: the same cut the `--set` arm applies.
            members: {
                let all: &[&str] = &vike_model::run_request::profile::PROFILE_TOP_LEVEL_KEYS;
                all.split_at(all.len() - 1).0
            },
            refused_members: &[],
            match_rule: "exact",
            derived_from: Some(
                "vike_model::run_request::profile::PROFILE_TOP_LEVEL_KEYS, less its last key (inputs)",
            ),
            admission: None,
            evidence: "crates/vike-model/src/run_request/profile.rs's `PROFILE_TOP_LEVEL_KEYS` (every top-level key `ProfileBacktest` takes, in its declaration order; `base_dir` is `#[serde(skip)]`, not a TOML key) less `inputs`, whose keys are series names (`7203.T`) that `--set` would split on `.`; the same cut is made in crates/vike-cli/src/cmd/backtest/run_args.rs (the `--set` loop of `RunCommand::into_args`), and it is the FIRST-SEGMENT roster only. `paramscan` is the one grid row: owner ruling 20 (phase 4) removed the serde alias that kept `sweep` loading, so `--set sweep.fast=…` is a usage error like any undeclared key.",
        },
        RosterRow {
            id: "rank_metrics",
            members: &vike_model::run_request::flag_vocab::RANK_METRICS,
            refused_members: &[
                "multi :: ⚠ NOT refused by today's engine-side `--rank-by` parser. Both routes resolve through crates/vike-backtest/src/search/select.rs's `resolve_rank`, whose FIRST arm is `Some(s) if s.eq_ignore_ascii_case(\"multi\")` — the engine binary reaches it via `parse_search_flags` and the backtesthub via `run_paramscan_profile`, so `--rank-by multi` is accepted on `--local` and on `--addr` alike (crates/vike-cli/tests/cli_verbs/search_walkforward_cli.rs's `every_search_knob_now_reaches_the_dial` drives `[\"--rank-by\", \"multi\"]` to a CONNECT failure, exit 3, proving it was not a usage error). It IS refused by the FOUR-ARM parser crates/vike-backtest/src/search/paramscan/rank.rs's `RankMetric::from_str_ci`, which returns None for it — and that parser is what a daemon predating the `search_method` capability uses, and what crates/vike-backtest/src/run/profile/walkforward_config.rs's `ProfileWalkforward::rank_metric` uses for the PROFILE key `[walkforward].rank_by`. Verbatim from that profile-key path: `unknown walkforward.rank_by \"multi\" (want sharpe | return | max_dd | equity; absent = sharpe)`. Against an old daemon the CLI never gets that far — crates/vike-datahub-client/src/client.rs's `run_paramscan_profile` pre-refuses `rank_by == \"multi\"` locally under FEATURE_SEARCH_METHOD, without sending.",
            ],
            match_rule: "ascii_ci",
            derived_from: Some("vike_model::run_request::flag_vocab::RANK_METRICS"),
            admission: None,
            evidence: "crates/vike-model/src/run_request/flag_vocab.rs's `RANK_METRICS` — this row's `members` field IS that const, so the published roster and the checked roster cannot differ — and its `accept_value`, which the CLI's `RunCommand::into_args` calls for `--rank-by`; crates/vike-backtest/src/search/select.rs's `resolve_rank`; crates/vike-backtest/src/search/paramscan/rank.rs's `RankMetric::from_str_ci`; crates/vike-backtest/src/run/profile/walkforward_config.rs's `ProfileWalkforward::rank_metric`; crates/vike-backtesthub-client/src/client.rs's `run_paramscan_profile`. ⚠ This row admitted `HAND-WRITTEN … the protocol crate exports no rank roster`, and BOTH halves have gone: the protocol crate exports one (it is the crate `vike-cli` and `vike-backtest` both depend on, which is the placement argument `SEARCH_METHODS` already settled), and `match` was `exact` while the engine resolved with `eq_ignore_ascii_case` — so `--rank-by SHARPE` was a local exit-2 and a legal, tested engine value at once. The admission's surviving observation is why the row reads the way it does: the first four members are the names `RankMetric::from_str_ci` accepts and the fifth, `multi`, is the composite objective `resolve_rank` special-cases above that parser, which is also why the const is named for the FLAG rather than for the metrics.",
        },
        RosterRow {
            id: "read_subcommands",
            members: &READ_SUBCOMMAND_WORDS,
            refused_members: &[],
            match_rule: "exact",
            derived_from: Some("crates/vike-cli/src/cmd/backtest/read.rs's READ_SUBCOMMANDS"),
            admission: None,
            evidence: "crates/vike-cli/src/cmd/backtest/read.rs's `READ_SUBCOMMANDS`, rendered through `ReadSub::as_str` at compile time (this row's `members`); `clap` claims the typed sub-verb word against the same names.",
        },
        RosterRow {
            id: "retired_top_level_commands",
            members: kept(crate::RETIRED_COMMANDS.iter().map(|(name, _)| *name)),
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
            refused_members: kept([format!(
                "<id>#N :: {}",
                crate::cmd::backtest::read::selector::refuse_a_child_form("{sel}", "{head}")
            )]),
            match_rule: "exact",
            derived_from: Some(
                "crates/vike-cli/src/cmd/backtest/read/selector.rs's module `//!` table",
            ),
            admission: None,
            evidence: "The `//!` table at the head of crates/vike-cli/src/cmd/backtest/read/selector.rs. It carries ONE row FORMS does not: the `<id>#N` child form, documented deliberately because it is RECOGNISED and refused rather than silently missing. crates/vike-cli/src/cmd/backtest/read/selector.rs's the_doc_table_and_the_refusal_agree pins both directions.",
        },
        RosterRow {
            id: "selector_forms",
            members: kept(crate::cmd::backtest::read::selector::FORMS.split(" | ")),
            refused_members: &[],
            match_rule: "exact",
            derived_from: Some("crates/vike-cli/src/cmd/backtest/read/selector.rs's FORMS"),
            admission: None,
            evidence: "crates/vike-cli/src/cmd/backtest/read/selector.rs's `FORMS`, which crates/vike-cli/src/cmd/backtest/read.rs's `required_selector` prints whole.",
        },
        RosterRow {
            id: "selector_refusal_forms",
            members: kept(crate::cmd::backtest::read::selector::FORMS.split(" | ")),
            refused_members: &[],
            match_rule: "exact",
            derived_from: Some("crates/vike-cli/src/cmd/backtest/read/selector.rs's FORMS"),
            admission: None,
            evidence: "crates/vike-cli/src/cmd/backtest/read.rs's required_selector, whose refusal (`backtest {} requires a run selector — one of: {FORMS}`) prints crates/vike-cli/src/cmd/backtest/read/selector.rs's FORMS whole. It fires on `backtest show|tag|gate|path|diff` with no positional (or a whitespace-only one). It printed four of the five until `@<mark>` was read from FORMS too.",
        },
        RosterRow {
            id: "show_export_values",
            members: kept(
                crate::cmd::backtest::read::show::EXPORTABLE
                    .into_iter()
                    .chain([crate::cmd::backtest::read::show::UNSTORED_EXPORT]),
            ),
            refused_members: kept([format!(
                "{} :: {}",
                crate::cmd::backtest::read::show::UNSTORED_EXPORT,
                crate::refusals::FlagRefusal::ExportFills
            )]),
            match_rule: "exact",
            derived_from: Some(
                "crates/vike-cli/src/cmd/backtest/read/show.rs's EXPORTABLE (the served half) plus export_document's `fills` arm",
            ),
            admission: None,
            evidence: "crates/vike-cli/src/cmd/backtest/read/show.rs's `EXPORTABLE` and `export_document` (the value is `spec.trim()`ed, then matched by exact string)",
        },
        RosterRow {
            id: "sort_directions",
            members: kept(
                crate::cmd::backtest::read::ls::SORT_DIRECTIONS.map(|(direction, _)| direction),
            ),
            refused_members: &[],
            match_rule: "exact",
            derived_from: Some("crates/vike-cli/src/cmd/backtest/read/ls.rs's sort_rows"),
            admission: None,
            evidence: "crates/vike-cli/src/cmd/backtest/read/ls.rs's `sort_rows` (the `spec.split_once(':')` match, which accepts only the literals \"asc\" and \"desc\")",
        },
        RosterRow {
            id: "top_level_commands",
            members: &TOP_LEVEL_COMMAND_WORDS,
            refused_members: &[],
            match_rule: "exact",
            derived_from: Some(
                "crates/vike-cli/src/lib.rs's `COMMANDS` (name, one-line summary) — the source for both `print_help` and the unknown-command hint, and the array the skills generator renders every `skills/*/SKILL.md` verb table from",
            ),
            admission: None,
            evidence: "crates/vike-cli/src/lib.rs's `COMMANDS`, its names taken at compile time (this row's `members`); each name has a matching arm in `dispatch`.",
        },
        RosterRow {
            id: "unbuilt_renderers",
            members: &crate::cmd::backtest::read::show::UNBUILT_RENDERERS,
            // ⚠ A copy of `refuse_an_unbuilt_renderer`'s sentences, held to it by
            // `surface_tests.rs`'s `the_unbuilt_renderer_messages_are_the_ones_the_binary_prints`:
            // rendered from `UNBUILT_RENDERERS` they would come out in that const's order, which the
            // published document does not use.
            refused_members: &[
                "--drawdowns :: --drawdowns is not available yet — it needs its WIRING here, not a renderer: `vike_analytics::periods::drawdown_table` folds the episodes and `drawdown_table_text` renders the table, both linked into this binary, and the EQUITY CURVE they read is in the run record (series.json). What is missing is a `--drawdowns N` value on this verb and a call to them. `--export equity` hands you the curve today, and `vike-cli report run:<run>` recomputes its single WORST drawdown. What `show` can render today: --metrics, --trades, --config, --export trades|equity, --json, --out.",
                "--breakdown :: --breakdown is not available yet — it needs the PER-BAR RETURNS, and a run to test the reason against. The stored equity curve is DECIMATED once a run exceeds the sample cap (series.json's `stride` above 1 means samples were dropped), so a period breakdown computed from it would disagree with report.json's own numbers — but this guard reads the command line alone, before any run is resolved, so it cannot tell whether YOUR run was thinned. `vike-cli report run:<run> --breakdown day|month` computes the breakdown from the same series.json and refuses only the runs that really were. What `show` can render today: --metrics, --trades, --config, --export trades|equity, --json, --out.",
                "--attribution :: --attribution is not available yet — it needs a per-source attribution the engine does not compute at all. What `show` can render today: --metrics, --trades, --config, --export trades|equity, --json, --out.",
            ],
            match_rule: "exact",
            derived_from: Some("crates/vike-cli/src/cmd/backtest/read/show.rs's UNBUILT_RENDERERS"),
            admission: None,
            evidence: "crates/vike-cli/src/cmd/backtest/read/show.rs's `UNBUILT_RENDERERS` and `refuse_an_unbuilt_renderer`; declared hidden on EVERY reading sub-verb, not just `show`, by crates/vike-cli/src/cmd/backtest/read.rs's `HiddenRefusals`, and refused by name before anything else in `ReadCommand::into_args`",
        },
        RosterRow {
            id: "where_operators",
            members: &crate::cmd::backtest::read::where_expr::OPS,
            refused_members: &[],
            match_rule: "exact",
            derived_from: Some("crates/vike-cli/src/cmd/backtest/read/where_expr.rs's OPS"),
            admission: None,
            evidence: "crates/vike-cli/src/cmd/backtest/read/where_expr.rs's `OPS` (declared LONGEST FIRST — the order is the parse) and `OP_HELP`",
        },
        RosterRow {
            id: "gap_dispositions",
            members: &vike_model::run_request::gap::OnGap::NAMES,
            refused_members: &[],
            match_rule: "ascii_ci",
            derived_from: Some("vike_model::run_request::gap::OnGap::NAMES"),
            admission: None,
            evidence: "crates/vike-cli/src/cmd/data.rs's `on_gap_override` (the parser this CLI actually consults, which lower-cases first — hence `ascii_ci`, and which spells its refusal from the same array); crates/vike-model/src/run_request/gap.rs's `OnGap`, which the engine's `ProfileData::on_gap` in crates/vike-backtest/src/run/profile/data_cfg.rs resolves through and refuses with; crates/vike-backtest/src/run/data_plan/verdict.rs's `CoverageVerdict`",
        },
        RosterRow {
            id: "universe_modes",
            members: &vike_model::run_request::gap::UniverseMode::NAMES,
            refused_members: &[],
            match_rule: "ascii_ci",
            derived_from: Some("vike_model::run_request::gap::UniverseMode::NAMES"),
            admission: None,
            evidence: "crates/vike-cli/src/cmd/data.rs's `universe_override` (which lower-cases first — hence `ascii_ci`, and which spells its refusal from the same array); crates/vike-model/src/run_request/gap.rs's `UniverseMode`, which the engine's `ProfileData::universe_mode` in crates/vike-backtest/src/run/profile/data_cfg.rs resolves through and refuses with; crates/vike-backtest/src/run/data_plan/verdict.rs's `universe_verdict`",
        },
    ]
});

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
        "sub_verbs": sub_verbs(),
        "sub_verb_required": true,
        "grammar": {
            "value_forms": ["--flag value", "--flag=value"],
            "split_on_first_equals_only": true,
            "help_stream": "stdout",
            "help_exit": 0,
        },
        "flags": flags,
        "rosters": rosters,
        "commands": commands::rows(),
    });
    let mut s = serde_json::to_string_pretty(&doc).expect("the surface document is plain data");
    s.push('\n');
    s
}

fn flag_json(f: &FlagRow) -> serde_json::Value {
    serde_json::json!({
        "long": f.long,
        "value": match f.value() {
            Value::None => "none",
            Value::Required => "required",
        },
        "value_name": f.published_value_name(),
        "sample": f.sample,
        "repeat": match f.repeat() {
            Repeat::Refused => "refused",
            Repeat::Accumulate => "accumulate",
            Repeat::CommaListFold => "comma_list_fold",
            Repeat::Idempotent => "idempotent",
        },
        "applies_to": f.accepted_on(),
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
            .map(|r| {
                serde_json::json!({
                    "id": r.id_word(),
                    "on": r.on,
                    "message": r.message(),
                })
            })
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
mod examples_tests;

#[cfg(test)]
mod row_steps_tests;

#[path = "surface_tests.rs"]
#[cfg(test)]
mod surface_tests;
