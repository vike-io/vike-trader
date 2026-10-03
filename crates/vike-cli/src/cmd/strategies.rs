//! The `backtest` plane's STRATEGY-AUTHORING sub-verbs — four of them, one socket between them.
//!
//! * `strategies` — the roster the COMPUTE DAEMON can run, over the wire (spec §8.3).
//! * `templates` — the starter Rhai strategies this BINARY ships, listed or emitted to a file.
//! * `script-api` — the complete callable surface of an authored Rhai strategy.
//! * `script-check` — compile one offline and answer with the diagnostic and an EXIT CODE.
//!
//! The last three dial nothing, read no store and locate no engine. They are in this module rather
//! than in `crate::cmd::runs` because they answer questions about a strategy that does not exist
//! yet — before there is a run to read — and `crate::cmd::runs`' whole identity is the run
//! directory.
//!
//! # ⚠ Why three sub-verbs and not three flags on `strategies`
//!
//! The rule this plane already runs on: a DIFFERENT OPERATION earns a sub-verb, a MODIFIER of one
//! operation stays a flag. `--json` is a modifier (same product, different rendering); `--rank-by`
//! is a modifier (same computation, different order). These three are not modifiers of "list what
//! the server can run": each has its own PRODUCT (template source text; a host-function roster; a
//! compile verdict), its own ARITY (one optional positional; none; none plus a required `--script`)
//! and its own failure surface — and `templates` WRITES A FILE, which no flag on a reading verb
//! does.
//!
//! The decisive precedent is `--list-params`. It was a MODE FLAG on `backtest` for months and
//! decision 11 of `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` re-homed it as
//! `backtest params`, because a discovery runs no backtest and every action inside this plane gets
//! exactly one spelling. Three more discoveries are three more sub-verbs by that same ruling;
//! folding them onto `strategies` as `--templates` / `--api` / `--check` would rebuild the shape
//! that ruling deleted, and would make one verb's `--addr` meaningless on three of its four modes.
//!
//! # ⚠ `strategies` opens a socket, and the stage table says it does not
//!
//! `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`'s §17 stage-4 row reads
//! "Artifact-only; no daemon work", and §6's opener says the reading verbs open no socket. That
//! verb is in §8, not §6, and §8.3 is explicit about why: the roster reaches a shell today only
//! through the engine binary's own `--list`, which answers about the LOCAL build, not the server's.
//! An operator with `vike-cli` and a remote daemon cannot enumerate what `--strategy` may name —
//! even though the wire verb that answers it is already served. Building this offline would rebuild
//! the defect.
//!
//! # What cannot be linked, and what that buys
//!
//! `vike_backtest::harness::STRATEGIES` lives inside that crate's harness, and a normal
//! `vike-backtest` edge would drag `vike-exec` into a CLI whose identity is being light and
//! DataFusion-free. So the roster arrives as `vike_datahub_client::DatahubClient::list_strategies`,
//! which is already written, already served by `vike_backtest::compute_server`, and already exposed
//! through MCP's `tool_list_strategies` — the ONE missing piece was the shell-facing verb.
//!
//! `vike_datahub_client::proto`'s `plane_of` puts `Request::ListStrategies` on `Plane::Compute`
//! (:7880, ruling 7) and `required_scope` puts it under `VerbScope::Read` — it returns a
//! compile-time const roster, not store contents — so a key-less dial is legal and the authenticated
//! one negotiates the weaker scope. `backtest run` is Control by contrast, because it compiles
//! client-supplied Rhai on the server.
//!
//! # The address ladder is `crate::cmd::backtest::resolve_addr` and nothing else
//!
//! `--addr` → `config.backtest_addr` → `vike_config::DEFAULT_BACKTEST_ADDR`, folded in ONE function
//! that `crates/vike-cli/src/cmd/study.rs` already calls. Two copies could disagree about a blank
//! rung and aim one client at the wrong port.
//!
//! # ⚠ The three offline verbs exist because a RELEASE INSTALL HAS NO SOURCE TREE
//!
//! That is the same argument `crate::cmd::indicators` was built on, applied to the rest of the same
//! surface: on an installed binary "read `crates/vike-script/src/engine.rs`" names a file the user
//! does not have, and a roster copied into prose over-advertises the day a verb is added or
//! withdrawn. Every one of these three therefore DERIVES its answer from the code that does the
//! work — `vike_script::HOST_FN_NAMES` for the bound host functions, `vike_script::RHAI_INDICATORS`
//! plus `vike_script::line_accessors` for the indicator spellings, `crate::cmd::mcp`'s `TEMPLATES`
//! for the starters, and `vike_script::discover_params` for the compile — and none of them writes a
//! roster down.
//!
//! ⚠ **The defect all three close is the AGENT SURFACE BEING WIDER THAN THE HUMAN ONE.**
//! `crate::cmd::mcp`'s `tool_list_templates` and `tool_validate_strategy` have shipped these two
//! answers to an agent for months, over a protocol a person cannot type; a human with the same
//! binary had neither. They call into the same data and the same compile path, so the two surfaces
//! cannot answer differently.

use vike_datahub_client::DatahubClient;
use vike_node_proto::auth::Scope;

use crate::cmd::runs::Ctx;
use crate::exit::{CliError, CmdResult, Exit};

/// `{ addr, count, strategies }` — the roster and the daemon it came FROM, because "which server
/// answered" is half the question the moment more than one exists.
pub(crate) fn strategies_json(addr: &str, names: &[String]) -> String {
    let doc = serde_json::json!({
        "addr": addr,
        "count": names.len(),
        "strategies": names,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

pub(crate) fn run_strategies(ctx: &Ctx<'_>, cli_addr: Option<&str>, json: bool) -> CmdResult<()> {
    let addr = crate::cmd::backtest::resolve_addr(cli_addr, ctx.configured_addr);
    let mut client = match ctx.keys {
        Some(k) => DatahubClient::connect_authed(&addr, k, Scope::Read),
        None => DatahubClient::connect(&addr),
    }
    .map_err(|e| {
        CliError::connect(format!(
            "cannot connect to the backtest daemon at {addr}: {e} (start it with \
             `vike-backend backtest --addr`)"
        ))
    })?;
    let names = client.list_strategies()?;
    if json {
        println!("{}", strategies_json(&addr, &names));
    } else {
        // ⚠ ONE NAME PER LINE and nothing else — the same shape the engine's own `--list` prints,
        // so a script that piped one can pipe the other. The address goes in the `--json` document,
        // never on stdout in the plain mode.
        for name in &names {
            println!("{name}");
        }
    }
    Ok(())
}

// ---- templates ------------------------------------------------------------------------------

/// The id a shell types for one shipped starter, derived from its own LABEL.
///
/// ⚠ **The labels are not typeable and that is why this function exists.** `crate::cmd::mcp`'s
/// `TEMPLATES` keys its rows on `"SMA cross"`, `"RSI reversion"`, `"Donchian breakout"` — strings
/// with spaces in them, which is fine over a JSON protocol and hostile on a command line
/// (`templates "SMA cross"` is one shell-quoting mistake from naming two positionals). So the id is
/// the label lowercased with spaces hyphenated, DERIVED rather than tabulated: a starter added to
/// that const gets an id with no edit here, and no second roster can fall behind it.
///
/// ⚠ The id is the ONLY spelling [`find_template`] accepts. Matching the label as well would be two
/// names for one thing, which this plane's decision 11 exists to refuse; the `--json` document
/// carries both, so nothing is lost to a machine.
pub(crate) fn template_id(label: &str) -> String {
    label.to_ascii_lowercase().replace(' ', "-")
}

/// Every starter's `(id, label, source)`, in the order `crate::cmd::mcp`'s `TEMPLATES` declares it.
fn template_rows() -> Vec<(String, &'static str, &'static str)> {
    crate::cmd::mcp::TEMPLATES
        .iter()
        .map(|(label, code)| (template_id(label), *label, *code))
        .collect()
}

/// One starter by id, case-insensitively. `None` for a name no row carries — answered by
/// [`run_templates`] with the derived roster, never with a bare "not found".
fn find_template(id: &str) -> Option<(String, &'static str, &'static str)> {
    let want = id.trim().to_ascii_lowercase();
    template_rows().into_iter().find(|(row_id, _, _)| *row_id == want)
}

/// The starter source, ready to be written or printed.
///
/// ⚠ Each const in `crate::cmd::mcp` opens with a newline (`r#"` then the first statement on the
/// next line), so the raw text starts with a blank line. A file whose first line is empty reads as
/// a truncated write, and a `$(…)` of one is off by a line — so the leading whitespace comes off
/// here, once, for both the print and the write. The TRAILING newline is kept: a text file this
/// workspace writes ends with one.
fn template_source(code: &'static str) -> &'static str {
    code.trim_start()
}

/// `{ count, templates: [{ id, name, code }] }` — or, for one starter that was WRITTEN, the same
/// row plus the path.
///
/// ⚠ `name` carries the LABEL, byte-identical to what `crate::cmd::mcp`'s `tool_list_templates`
/// emits under that key, so an agent and a human reading the two documents cannot come to disagree
/// about what a starter is called. `id` is ADDITIVE — the typeable spelling this verb accepts — and
/// the agent document is deliberately left alone rather than grown to match.
pub(crate) fn templates_json(rows: &[(String, &'static str, &'static str)]) -> String {
    let templates: Vec<serde_json::Value> = rows
        .iter()
        .map(|(id, label, code)| {
            serde_json::json!({ "id": id, "name": label, "code": template_source(code) })
        })
        .collect();
    let doc = serde_json::json!({ "count": templates.len(), "templates": templates });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

/// `vike-cli backtest templates [ID] [--write-strategy FILE] [--json]`.
///
/// Three products from one verb, and each is the same question at a different grain: with no id it
/// is the ROSTER, with an id it is that starter's SOURCE, and with `--write-strategy` it is the
/// PATH it was saved to. Freqtrade's `new-strategy --template`, LEAN's `project-create` and Jesse's
/// `make-strategy` are the competitor shapes; the roster half is what none of them prints and what
/// this tree needs, because the starters are a const rather than a directory a user can list.
///
/// ⚠ **`--write-strategy` REFUSES AN EXISTING PATH**, which is the whole reason it exists beside a
/// shell redirect. `> my.rhai` clobbers, and the file it clobbers is a strategy somebody has been
/// editing — unrecoverable, from a flag typo. It is `--write-profile`'s rule
/// (`crate::cmd::backtest`'s `Args::write_profile`) for the same reason, and it is why `--out` was
/// NOT reused: that flag OVERWRITES on `ls` and `show`, and one flag with two clobber policies is a
/// trap whichever way a reader guesses.
pub(crate) fn run_templates(id: Option<&str>, write: Option<&str>, json: bool) -> CmdResult<()> {
    let Some(id) = id.map(str::trim).filter(|s| !s.is_empty()) else {
        // ⚠ A `--write-strategy` with nothing to write is a USAGE refusal rather than a roster
        // print: the operator asked for a file and would otherwise get a listing and exit 0, which
        // reads exactly like a write that happened.
        if let Some(path) = write {
            return Err(CliError::usage(format!(
                "--write-strategy {path} needs a starter to write — name one: `vike-cli backtest \
                 templates <id> --write-strategy {path}`. The ids are: {}",
                template_roster()
            )));
        }
        let rows = template_rows();
        if json {
            println!("{}", templates_json(&rows));
        } else {
            // ⚠ ONE ID PER LINE and nothing else — `run_strategies` above states the rule and this
            // is the same kind of answer, so a script that piped one can pipe the other. The LABEL
            // goes in the `--json` document; a second column here would break every pipe.
            for (row_id, _, _) in &rows {
                println!("{row_id}");
            }
        }
        return Ok(());
    };
    let (row_id, label, code) = find_template(id).ok_or_else(|| {
        CliError::usage(format!(
            "no starter strategy named '{id}'. The ids are: {}",
            template_roster()
        ))
    })?;
    let Some(path) = write else {
        if json {
            println!("{}", templates_json(&[(row_id, label, code)]));
        } else {
            // The SOURCE verbatim and nothing around it, so `templates sma-cross > s.rhai` is a
            // valid script. `print!` rather than `println!`: the trimmed text already ends with a
            // newline, and a second one is a blank line at the end of a written file.
            print!("{}", template_source(code));
        }
        return Ok(());
    };
    if std::path::Path::new(path).exists() {
        return Err(CliError::usage(format!(
            "--write-strategy {path} already exists, and this command will not overwrite a \
             strategy — delete it, or name a different path"
        )));
    }
    std::fs::write(path, template_source(code))
        .map_err(|e| CliError::failed(format!("cannot write the strategy to {path}: {e}")))?;
    if json {
        let doc = serde_json::json!({ "id": row_id, "name": label, "written": path });
        println!(
            "{}",
            serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
        );
    } else {
        // The PATH alone, so `$(vike-cli backtest templates sma-cross --write-strategy s.rhai)` is
        // usable — `crate::cmd::runs::path`'s rule, applied to the file this verb creates.
        println!("{path}");
    }
    Ok(())
}

/// The ids a refusal names, DERIVED from the same const the verb serves.
fn template_roster() -> String {
    template_rows().into_iter().map(|(id, _, _)| id).collect::<Vec<_>>().join(", ")
}

// ---- script-api -----------------------------------------------------------------------------

/// Which family a row belongs to — the four blocks `script-api` prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostKind {
    /// A zero-argument hook the SCRIPT defines and the host CALLS. The one family that is not part
    /// of the callable surface, and the one a listing is useless without: the reads and the verbs
    /// have to be written somewhere.
    Hook,
    /// A snapshot read — `crates/vike-script/src/engine.rs`'s `register_reads`.
    Read,
    /// An order verb — `crates/vike-script/src/engine.rs`'s `register_verbs`.
    Order,
    /// `param(name, default)`, registered by `build_engine` itself.
    Knob,
}

impl HostKind {
    /// The block heading, and the `kind` string the `--json` document carries.
    fn heading(self) -> &'static str {
        match self {
            HostKind::Hook => "hooks you define (zero-argument, each optional)",
            HostKind::Read => "reads",
            HostKind::Order => "order verbs",
            HostKind::Knob => "knobs",
        }
    }

    fn json_name(self) -> &'static str {
        match self {
            HostKind::Hook => "hook",
            HostKind::Read => "read",
            HostKind::Order => "order",
            HostKind::Knob => "knob",
        }
    }
}

/// One callable (or, for [`HostKind::Hook`], one definable) name, with the shape a script types.
pub(crate) struct HostFn {
    /// The name. For everything but a hook this is a `vike_script::HOST_FN_NAMES` entry, and
    /// [`the_rows_are_exactly_the_host_bound_names`] holds that true in both directions.
    pub(crate) name: &'static str,
    pub(crate) kind: HostKind,
    /// The call form, argument names included — what a reader copies.
    pub(crate) call: &'static str,
    /// What comes back, in the Rhai types a script sees. `nothing` for a verb and a hook.
    pub(crate) returns: &'static str,
    /// One line on what it means, or what it refuses.
    pub(crate) note: &'static str,
}

/// THE describing table — one row per `vike_script::HOST_FN_NAMES` entry, plus the three hooks.
///
/// ⚠ **This is a describing table over an exported roster, not a second roster.** The NAMES are
/// `vike_script::HOST_FN_NAMES`, which `crates/vike-script/src/engine.rs`'s
/// `register_reads`/`register_verbs` are held to by that crate's `host_fn_names_are_all_callable`;
/// what lives here is the prose that belongs to a RENDERER — the argument names, the return type
/// and the sentence. [`the_rows_are_exactly_the_host_bound_names`] fails on a row for a name the
/// host does not bind AND on a bound name with no row, so this table cannot over- or
/// under-advertise. It is the same shape as a per-venue capability map: one row per member of a
/// canonical roster, plus a completeness test that iterates the roster.
///
/// The signatures were read from the registrations — `register_reads` binds eight `|| -> f64`
/// getters plus `index`/`now` as `|| -> i64`; `register_verbs` binds `market(i64, f64)`,
/// `buy(f64)`, `sell(f64)` and `limit(i64, f64, f64)`; `build_engine` binds
/// `param(&str, Dynamic) -> f64`. A `side` is `1` or `-1`, which is how `register_verbs` casts it
/// into `vike_script`'s `Intent`.
///
/// ⚠ **THE THREE HOOK ROWS ARE AN UNGATED HAND COPY, and that is admitted rather than hidden.**
/// `vike_script::RhaiStrategy`'s `compile_with_indicators` detects them with three string literals
/// and exports no roster of them, so nothing can hold these rows equal to it: a hook renamed there
/// leaves this listing confidently wrong. They are carried anyway because a callable surface with
/// no answer to "where do I put this call" sends the reader to the templates to guess, and
/// `crate::cmd::mcp`'s `TEMPLATES` const sets the precedent for a copy declared in writing. The
/// completeness gate deliberately SKIPS them — a hook is not a name the script calls, so
/// `HOST_FN_NAMES` neither carries one nor should.
pub(crate) const HOST_FNS: &[HostFn] = &[
    HostFn {
        name: "on_start",
        kind: HostKind::Hook,
        call: "fn on_start()",
        returns: "nothing",
        note: "once, before the first bar. A hook the script does not define is skipped entirely, never attempted.",
    },
    HostFn {
        name: "on_bar",
        kind: HostKind::Hook,
        call: "fn on_bar()",
        returns: "nothing",
        note: "once per bar — where an indicator call has to live, because a conditionally-called indicator silently skips the bars it was not called on.",
    },
    HostFn {
        name: "on_stop",
        kind: HostKind::Hook,
        call: "fn on_stop()",
        returns: "nothing",
        note: "once, after the last bar.",
    },
    HostFn {
        name: "close",
        kind: HostKind::Read,
        call: "close()",
        returns: "f64",
        note: "the current bar's close.",
    },
    HostFn {
        name: "open",
        kind: HostKind::Read,
        call: "open()",
        returns: "f64",
        note: "the current bar's open.",
    },
    HostFn {
        name: "high",
        kind: HostKind::Read,
        call: "high()",
        returns: "f64",
        note: "the current bar's high.",
    },
    HostFn {
        name: "low",
        kind: HostKind::Read,
        call: "low()",
        returns: "f64",
        note: "the current bar's low.",
    },
    HostFn {
        name: "volume",
        kind: HostKind::Read,
        call: "volume()",
        returns: "f64",
        note: "the current bar's volume.",
    },
    HostFn {
        name: "position",
        kind: HostKind::Read,
        call: "position()",
        returns: "f64",
        note: "signed position size — negative is short. This is how a script observes a fill; there is no fill callback.",
    },
    HostFn {
        name: "price",
        kind: HostKind::Read,
        call: "price()",
        returns: "f64",
        note: "the mark the engine is pricing against.",
    },
    HostFn {
        name: "equity",
        kind: HostKind::Read,
        call: "equity()",
        returns: "f64",
        note: "account equity, for sizing off the book rather than off a constant.",
    },
    HostFn {
        name: "index",
        kind: HostKind::Read,
        call: "index()",
        returns: "i64",
        note: "the bar's ordinal — 0 on the first bar, for a warm-up guard that does not depend on an indicator returning NaN.",
    },
    HostFn {
        name: "now",
        kind: HostKind::Read,
        call: "now()",
        returns: "i64",
        note: "the bar's timestamp in epoch milliseconds.",
    },
    HostFn {
        name: "market",
        kind: HostKind::Order,
        call: "market(side, qty)",
        returns: "nothing",
        note: "side is 1 to buy and -1 to sell. Recorded as an intent and drained into a real order after the hook returns.",
    },
    HostFn {
        name: "buy",
        kind: HostKind::Order,
        call: "buy(qty)",
        returns: "nothing",
        note: "market(1, qty).",
    },
    HostFn {
        name: "sell",
        kind: HostKind::Order,
        call: "sell(qty)",
        returns: "nothing",
        note: "market(-1, qty).",
    },
    HostFn {
        name: "limit",
        kind: HostKind::Order,
        call: "limit(side, qty, price)",
        returns: "nothing",
        note: "a resting limit order at price; same side convention as market().",
    },
    HostFn {
        name: "param",
        kind: HostKind::Knob,
        call: "param(name, default)",
        returns: "f64",
        note: "a sweepable knob. Call it at the TOP LEVEL, not inside a hook: the top level runs exactly once, which is what bakes a swept value in. `vike-cli backtest params --script` lists what a script declares.",
    },
];

/// The four blocks, in print order.
const HOST_KIND_ORDER: [HostKind; 4] =
    [HostKind::Hook, HostKind::Read, HostKind::Order, HostKind::Knob];

/// Every INDICATOR spelling a script can actually type, built-ins then the user's own.
///
/// ⚠ **Spellings, not indicator names, and the difference is the whole point.** `bollinger` is
/// absent from `vike_script::RHAI_INDICATORS` — its bare name is refused, because line 0 is the
/// upper band — while `bollinger_mid(20)` is perfectly callable. So a bare name is printed only
/// when the host binds it, and every per-line accessor is printed beside it. Printing a name the
/// host does not bind is the one failure this listing must not have: rhai resolves a function name
/// when the line RUNS, so a script naming an unbound indicator compiles, raises on every bar and
/// self-disables after the consecutive-error cap — mounted, and silently never trading.
///
/// The derivation is `crate::cmd::indicators`' (`bound_metas` for the registry half, `user_rows`
/// for the installed user files), so this verb and `vike-cli indicators` cannot disagree about what
/// is callable. What this one deliberately does NOT carry is the parameters and the categories:
/// that is the other verb's product, and a second rendering of it here would be the copy both
/// modules' docs argue against.
pub(crate) fn callable_indicator_spellings() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in crate::cmd::indicators::bound_metas() {
        if vike_script::RHAI_INDICATORS.contains(&m.name) {
            out.push(m.name.to_string());
        }
        for (_, call) in vike_script::line_accessors(m.name) {
            out.push(call);
        }
    }
    for r in crate::cmd::indicators::user_rows() {
        if r.bare_call {
            out.push(r.name.to_string());
        }
        for (_, call) in r.accessors {
            out.push(call);
        }
    }
    out
}

/// `{ functions: [{ name, kind, call, returns, note }], indicators: { count, callable } }`.
///
/// ONE array with a `kind` discriminator rather than four keyed blocks — `crate::cmd::params`'
/// `source` field is the in-crate precedent: one document a consumer can iterate beats several
/// nobody can tell apart.
pub(crate) fn host_api_json(indicators: &[String]) -> String {
    let functions: Vec<serde_json::Value> = HOST_FNS
        .iter()
        .map(|f| {
            serde_json::json!({
                "name": f.name,
                "kind": f.kind.json_name(),
                "call": f.call,
                "returns": f.returns,
                "note": f.note,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "functions": functions,
        "indicators": { "count": indicators.len(), "callable": indicators },
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

/// `vike-cli backtest script-api [--json]` — the whole callable surface, offline.
///
/// PURE apart from the print: it opens no file, dials nothing and reads no clock. The user-indicator
/// half reads PROCESS state that `crate::install_user_indicators` populated at the composition root
/// before any sub-verb ran, exactly as `crate::cmd::indicators` does — a listing that re-scanned the
/// folder could print an indicator this process never installed and therefore cannot call.
pub(crate) fn run_script_api(json: bool) -> CmdResult<()> {
    let indicators = callable_indicator_spellings();
    if json {
        println!("{}", host_api_json(&indicators));
        return Ok(());
    }
    print!("{}", render_host_api(&indicators));
    Ok(())
}

/// The human listing. PURE (returns the text rather than printing it), the shape
/// `crate::cmd::indicators`' `render_user_rows` has, so a test reads what a user reads.
pub(crate) fn render_host_api(indicators: &[String]) -> String {
    let mut out = String::new();
    for kind in HOST_KIND_ORDER {
        let rows: Vec<&HostFn> = HOST_FNS.iter().filter(|f| f.kind == kind).collect();
        if rows.is_empty() {
            continue;
        }
        out.push_str(kind.heading());
        out.push('\n');
        let width = rows.iter().map(|f| f.call.len()).max().unwrap_or(0).min(28);
        for f in rows {
            out.push_str(&format!("  {:<width$}  {}  {}", f.call, f.returns, f.note));
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "indicators — {} callable spellings. `vike-cli indicators` prints their parameters, their \
         defaults and why a registry name that is missing is missing.\n",
        indicators.len()
    ));
    out.push_str(&wrap_names(indicators, 92, "  "));
    // ⚠ The three language-level facts a reader needs and no roster above can carry. They are rhai
    // properties rather than rows of ours, which is why they are prose here and not a table: they
    // cannot rot when a host function is added or withdrawn.
    out.push_str(
        "\nrhai's own standard library is available on top of this (arithmetic, `abs`, `to_int`, \
         `is_nan`, …) and is the LANGUAGE rather than this host's surface, so it is not listed \
         here.\n`print`/`debug` are redirected to the log, never to stdout — the host's stdout is a \
         protocol.\n`import` is REFUSED outright: a script may not read a file, and the settings \
         tree is what it would be reading.\n",
    );
    out
}

/// Comma-join `names` into lines no wider than `width`, each prefixed with `indent`.
///
/// Written rather than reached for: this crate carries no text-wrapping dependency, deliberately,
/// and the alternative — one name per line — turns a near-whole indicator catalog into a listing
/// nobody scrolls to the end of.
fn wrap_names(names: &[String], width: usize, indent: &str) -> String {
    let mut out = String::new();
    let mut line = String::new();
    for (i, name) in names.iter().enumerate() {
        let last = i + 1 == names.len();
        let piece = if last { name.clone() } else { format!("{name},") };
        if !line.is_empty() && indent.len() + line.len() + 1 + piece.len() > width {
            out.push_str(indent);
            out.push_str(&line);
            out.push('\n');
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&piece);
    }
    if !line.is_empty() {
        out.push_str(indent);
        out.push_str(&line);
        out.push('\n');
    }
    out
}

// ---- script-check ---------------------------------------------------------------------------

/// `{ script, ok, error, params }` — the MCP tool's two keys plus the file it judged.
///
/// ⚠ `ok` and `error` are spelled exactly as `crate::cmd::mcp`'s `tool_validate_strategy` spells
/// them, because the two surfaces answer one question off one compile and a consumer should not
/// have to learn which one it is talking to. `params` is the COUNT the compile already produced,
/// never the list: `vike-cli backtest params --script` is the verb whose product is the list, and a
/// second copy of that document here would be two answers to one question.
pub(crate) fn check_json(path: &str, error: Option<&str>, params: usize) -> String {
    let doc = serde_json::json!({
        "script": path,
        "ok": error.is_none(),
        "error": error,
        "params": params,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

/// `vike-cli backtest script-check --script <s.rhai> [--json]` — compile it, offline, and put the
/// answer in the EXIT CODE.
///
/// # ⚠ The rung is `Failed` (1), and the two it is deliberately not
///
/// * NOT `Usage` (2). Rung 2 means the command line was wrong and re-running unchanged cannot
///   succeed. A script that does not compile was named correctly; the file is what has to change.
///   A MISSING `--script` is a genuine rung 2, and is the only one this verb produces.
/// * NOT `Breach` (6). That rung is a THRESHOLD verdict and
///   `crates/vike-cli/src/cmd/runs/gate.rs`'s `rung` is its one producer; a compile failure is not
///   a breached criterion, and minting a second producer would blur the one rung a CI step
///   escalates on.
///
/// `1` is what `backtest params --script <bad>` already exits with for this very diagnostic
/// (`crate::cmd::params`' `run_script`), so one compile error has one number. A save hook or a
/// pre-commit reads any non-zero; what it must not have to do is tell a bad script from a bad
/// command line, and the ladder is what separates them.
///
/// # ⚠ Why the verdict goes to STDOUT and not through `CliError`
///
/// `crate::cmd::runs::gate` argues it first: routing a verdict through `CliError` prints one stderr
/// line and throws the `--json` document away. This verb's product IS the diagnostic, so it is
/// printed in full on stdout in both modes and the rung is computed beside it — which is also why
/// this is the second arm of `execute_read` that returns an `Exit` rather than `()`.
///
/// # ⚠ It compiles through `vike_script::discover_params` rather than a bare `compile`
///
/// Which is not an accident of reuse: that path compiles the script AND runs its top level exactly
/// once, which is the same error surface mounting the strategy has
/// (`vike_script::RhaiStrategy::compile_with_indicators` does the same one-time run). A bare
/// `compile` would pass a script whose `param()` call raises, so "it checked" would mean less than
/// the operator thinks. `crate::cmd::mcp`'s `tool_validate_strategy` reaches the same function for
/// the same reason, so the agent and the human cannot get different verdicts.
pub(crate) fn run_script_check(script: Option<&str>, json: bool) -> CmdResult<Exit> {
    let path = script.map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| {
        CliError::usage(
            "`backtest script-check` needs a script: --script <s.rhai>. It compiles one file \
             offline and answers in the exit code — no server, no store, no engine.",
        )
    })?;
    // An unreadable file is not a verdict about the script: nothing was compiled, so there is
    // nothing to print a diagnostic about. Same rung and same sentence `crate::cmd::params`' own
    // read uses, because it is the same failure.
    let src = std::fs::read_to_string(path)
        .map_err(|e| CliError::failed(format!("cannot read script {path}: {e}")))?;
    match vike_script::discover_params(&src) {
        Ok(params) => {
            if json {
                println!("{}", check_json(path, None, params.len()));
            } else {
                println!("ok: {path} compiles ({} param() knob(s))", params.len());
            }
            Ok(Exit::Ok)
        }
        Err(e) => {
            let diag = e.to_string();
            if json {
                println!("{}", check_json(path, Some(&diag), 0));
            } else {
                println!("error: {path} does not compile");
                println!("{diag}");
            }
            Ok(Exit::Failed)
        }
    }
}

#[path = "strategies_tests.rs"]
#[cfg(test)]
mod strategies_tests;
