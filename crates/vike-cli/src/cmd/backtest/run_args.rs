//! The `run` grammar: the top-level keys, the sugar table, the `params` refusals and the parser.

use vike_datahub_client::flag_vocab;

use crate::cmd::args::{self, Flags};

#[cfg(doc)]
use super::profile::set_profile_key;
use super::profile::{looks_like_a_search_axis, parse_scalar};
use super::{Args, LIST_PARAMS_RETIRED, Origin, Override};

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
pub(super) const PARAMS_REFUSED: &[&str] = &[
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
pub(super) fn refuse_a_run_flag_on_params(flag: &str) -> String {
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
pub(super) fn parse_run_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
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
