//! `vike-cli indicators` — print the indicator functions a Rhai strategy can actually CALL.
//!
//! # Why a command, and not a sentence in a README
//!
//! The scaffolded `user_data/strategies/rhai/README.md`
//! (`crates/vike-cli/src/cmd/init/content/readmes.rs`'s `RHAI_README`) tells a user the host binds the
//! `vike-indicators` catalog rather than a handful of names. On a RELEASE install that claim is
//! unanswerable: there is no source tree, so "read `crates/vike-script/src/engine/bindable.rs`'s
//! `RHAI_INDICATORS`" names a file the user does not have, and a roster written into prose is
//! wrong the first time an indicator is added. So the roster is PRINTED, off the same derived list
//! the host binds from.
//!
//! `crates/vike-cli/src/cmd/mcp/offline_tools.rs`'s `tool_list_indicators` answers the identical question for an
//! AGENT. Both call [`bound_metas`] and render [`detail_row`], so the human surface and the machine
//! surface cannot disagree about what is callable — which is the only property that matters here,
//! because the cost of being wrong is silent.
//!
//! # Two kinds of thing, kept apart
//!
//! The rows above are the BUILT-IN catalog: shipped, categorised, identical on every install. A
//! user's own `user_data/indicators/<name>.rhai` is callable from the same script by the same
//! spelling — [`crate::install_user_indicators`] installs the set process-wide before any
//! subcommand runs — and it is a different kind of thing: authored on THIS box, with a file behind
//! it and no category to file it under. Listing only the built-ins told the author their own
//! indicator did not exist, which is the same "a name this command does not print is not callable"
//! reading the warning above trades on, pointed at the one indicator they are surest about.
//!
//! So they are printed in their own block, under their own heading, and `--json` carries them
//! under their own key with no `category` field — the distinction is structural in both, not a
//! label a reader has to notice. The DERIVATION is `vike_script::installed_user_indicators` joined
//! to `vike_script::installed_user_indicator_params` for the knobs and to
//! `installed_user_bare_call`/`installed_user_line_accessors` for the spellings that resolve;
//! nothing here reads a directory (the dispatcher already did) and nothing here re-derives a call
//! form. That last join is not decoration: a user file may declare `fn outputs()`, and one whose
//! line 0 is not its namesake has NO bare name, exactly as `bollinger` has none.
//!
//! ⚠ `--category` never matches one, and that is not an omission: a user indicator HAS no category,
//! so a filtered listing is a listing of built-ins by construction. `--name` does resolve one —
//! `find_bound`'s "no indicator named" would otherwise be a lie about a file on the user's own disk.
//!
//! ⚠ **One residual, DECLARED: `--name` matches the file's own name, not a per-line accessor.**
//! `--name my_bands` answers; `--name my_bands_mid` falls through to `find_bound` and reads "no
//! indicator named", which is wrong about a spelling this very command printed. It is narrower than
//! it looks FOR A USER INDICATOR — that block names every accessor, so the answer is one screen
//! away. ⚠ It is NOT narrower on the built-in side: the human roster names no accessor anywhere
//! (only `--json` carries them, per `render_rows`), so `--name bollinger_mid` misses AND the
//! spelling appears in no printed listing at all. Closing it is one change to `find_bound` for both
//! kinds rather than a user-side special case, and it matters more for the built-ins.
//!
//! ⚠ **`crates/vike-cli/src/cmd/mcp/offline_tools.rs`'s `tool_list_indicators` does NOT yet list them.** The
//! shared-derivation claim above is about the BUILT-IN set ([`bound_metas`]/[`detail_row`]), which
//! both surfaces still take from here; the user block is this command's alone today.
//!
//! # It prints the BOUND set, never the registry
//!
//! Every row is a name `vike_script::RHAI_INDICATORS` carries — what
//! `crates/vike-script/src/engine/builtin.rs`'s `register_indicators` actually registers — joined onto its
//! `vike_indicators::registry()` metadata for the label, the parameters and the output lines.
//! Printing a registry name the host does NOT bind is the defect the MCP tool was narrowed to fix:
//! rhai resolves a function name when the line RUNS, so a script naming an unbound indicator
//! compiles, raises on every bar, and self-disables after the consecutive-error cap — mounted, and
//! silently never trading. A name absent from this listing is a name that fails that way.
//!
//! # No number is written down
//!
//! The footer counts what it printed, and the help text states no total. The set is DERIVED; a
//! count in prose is a hand copy of a derivation, and this repo has paid for that one repeatedly.

use std::process::ExitCode;

use serde_json::{Value, json};
use vike_indicators::IndicatorMeta;

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};

const USAGE: &str = "\
usage: vike-cli indicators [options]

Print the indicators a Rhai strategy can call, grouped by category, each with its parameters and
the defaults you get by leaving them out. Offline: no store, no settings, no credential.

Your own indicators (user_data/indicators/*.rhai) are listed too, in their own block — they are
installed process-wide at startup and callable by the same spelling as a built-in.

options:
  --name NAME      just this one — or, if it is not callable, WHY not
  --category NAME  only this category (case-insensitive); built-ins only, yours have no category
  --json           machine-readable rows instead of the grouped listing
  -h, --help       this message

⚠ A name this command does NOT print is not callable. It still COMPILES — rhai resolves a function
name when the line runs — and then fails on every bar until the strategy switches itself off. Ask
`--name` about a missing one: a few registry indicators are held back on purpose, and it says which
reason applies rather than leaving you to guess at a typo.";

/// The parsed `indicators` command line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    /// `--name`: one indicator, or the reason it cannot be called. Mutually exclusive with
    /// `category` — a request for one specific thing and a request for a family cannot both be
    /// honoured, and silently ignoring one of them is how a listing lies.
    name: Option<String>,
    /// `--category`: narrow to one family, matched case-insensitively against the category name
    /// each row reports.
    category: Option<String>,
    json: bool,
}

fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut name = None;
    let mut category = None;
    let mut json = false;
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--name" => name = Some(flags.value(&flag, inline)?),
            "--category" => category = Some(flags.value(&flag, inline)?),
            "--json" => {
                no_value(&flag, inline)?;
                json = true;
            }
            "-h" | "--help" => return help_requested(),
            other => return Err(format!("unknown option '{other}'")),
        }
    }
    if name.is_some() && category.is_some() {
        return Err("--name and --category ask different questions; pass one".to_string());
    }
    Ok(Args { name, category, json })
}

/// THE derivation: every `vike_indicators::registry()` entry whose name the Rhai host binds, in
/// registry order (curated, stable — not alphabetical, so related indicators stay together).
///
/// Filtered by `vike_script::RHAI_INDICATORS` rather than reproducing it, so this listing follows
/// the binding automatically. `the_listing_is_exactly_the_host_bound_set` asserts BOTH directions,
/// including the one a filter hides: a bound name with no registry entry would silently vanish
/// from every roster while remaining callable.
pub(crate) fn bound_metas() -> Vec<&'static IndicatorMeta> {
    vike_indicators::registry()
        .iter()
        .filter(|m| {
            // Reachable under its BARE name, OR through at least one per-line accessor. The second
            // arm is what keeps `bollinger` in this listing: its bare name is refused (line 0 is the
            // upper band), but `bollinger_mid(20)` is callable, and a roster that omitted it would
            // tell a user that a band indicator is unreachable when it is not.
            vike_script::RHAI_INDICATORS.contains(&m.name)
                || !vike_script::line_accessors(m.name).is_empty()
        })
        .collect()
}

/// The heading the user's own block hangs under. Names the FOLDER, because the reader's next
/// action after seeing (or not seeing) a row here is to open it.
const USER_BLOCK_HEADING: &str = "your own (user_data/indicators)";

/// One installed user indicator, as the renderers take it: the name a script calls, the
/// `param(name, default)` knobs the file itself declares, and which SPELLINGS of it resolve.
///
/// A struct rather than a bare `&str` so the human block and the machine row are built from ONE
/// value — and so the tests can drive both with a synthetic row instead of installing into the
/// process, which is a `OnceLock` shared by every test in this binary.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UserRow {
    pub(crate) name: &'static str,
    pub(crate) params: Vec<(&'static str, f64)>,
    /// The CALLABLE per-line spellings, `(line, function)`, in the file's declaration order —
    /// EMPTY for a single-output file, exactly as `vike_script::line_accessors` is for a
    /// single-output built-in.
    pub(crate) accessors: Vec<(String, String)>,
    /// Whether the BARE name resolves. `false` with a non-empty [`UserRow::accessors`] is the band
    /// shape: reachable, but only line by line.
    pub(crate) bare_call: bool,
}

/// THE derivation for the user's own indicators: whatever a binary installed at startup
/// (`vike_script::installed_user_indicators`), each joined to its own declared knobs and to the
/// spellings that actually resolve.
///
/// Empty when nothing was installed — which is both the fresh-install state and every case where
/// the dispatcher found no project, so an empty block is simply not printed.
///
/// ⚠ This reads PROCESS state, not a directory. `crate::install_user_indicators` did the I/O once,
/// at the composition root, before any subcommand ran; a listing that re-scanned the folder could
/// print an indicator this process never installed and therefore cannot call.
pub(crate) fn user_rows() -> Vec<UserRow> {
    vike_script::installed_user_indicators()
        .into_iter()
        .map(|name| UserRow {
            name,
            params: vike_script::installed_user_indicator_params(name),
            accessors: vike_script::installed_user_line_accessors(name),
            bare_call: vike_script::installed_user_bare_call(name),
        })
        .collect()
}

/// Every spelling a script may TYPE for one user indicator, as `(call form, the line it returns)` —
/// each with the file's own knobs spelled out, the same widest-call form [`call_form`] prints for a
/// built-in and for the same reason: the host registers one form per argument count from zero up to
/// the declared count, so a row is simultaneously the widest call and the answer to "what do the
/// ones I leave out become".
///
/// ⚠ **The bare name is printed only when it BINDS.** A file declaring
/// `fn outputs() { ["upper", "mid", "lower"] }` has no bare name — the same refusal `bollinger`
/// carries, for the same reason (line 0 is the upper band) — and printing `my_bands(width=2)`
/// anyway would be printing a call that raises on every bar and self-disables the strategy after
/// the consecutive-error cap. That is precisely the silent failure this command's narrowing exists
/// to prevent, aimed at the one indicator the author wrote themselves. The line accessors are
/// printed instead, so the block always names a way IN.
///
/// ⚠ A file declaring no `param()` renders `name()`, WITH the empty parentheses, matching a
/// zero-parameter built-in. Printing a bare `name` would read as a value rather than a call.
fn user_call_forms(r: &UserRow) -> Vec<(String, Option<&str>)> {
    let params: Vec<String> =
        r.params.iter().map(|(n, d)| format!("{n}={}", fmt_num(*d))).collect();
    let args = params.join(", ");
    let mut out: Vec<(String, Option<&str>)> = Vec::new();
    if r.bare_call {
        out.push((format!("{}({args})", r.name), None));
    }
    for (line, call) in &r.accessors {
        out.push((format!("{call}({args})"), Some(line.as_str())));
    }
    out
}

/// The user's own block, or the EMPTY string when nothing is installed.
///
/// Empty rather than a heading over nothing: a "your own" header with no rows under it reads as a
/// build that lost them, and the ordinary state of an install is having written none.
///
/// PURE (returns the text rather than printing it), like [`render_rows`], so the tests read what a
/// user reads.
fn render_user_rows(rows: &[UserRow]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let forms: Vec<(String, Option<&str>)> = rows.iter().flat_map(user_call_forms).collect();
    let width = forms.iter().map(|(f, _)| f.len()).max().unwrap_or(0).min(40);
    let mut out = format!("{USER_BLOCK_HEADING}\n");
    for (form, line) in forms {
        // No label column — a user indicator has no `pretty` name to print — but the `-> line`
        // column is the same one `render_rows` gives a built-in, and it is what tells the author
        // which of their own numbers a per-line spelling hands back. The bare name gets none: it
        // binds only when line 0 is named after the file, so the name already says it.
        let returns = line.map(|l| format!("  -> {l}")).unwrap_or_default();
        out.push_str(format!("  {form:<width$}{returns}").trim_end());
        out.push('\n');
    }
    out
}

/// One user indicator as a machine row.
///
/// ⚠ Deliberately NOT [`detail_row`]'s shape. There is no `category` (there is nothing to file it
/// under), no `pretty` label and no `outputs` list — spelling those as empty or null would tell a
/// consumer the fields exist and happen to be blank. What it carries instead is `source`: the
/// provenance that makes this a different kind of thing from a shipped built-in.
///
/// ⚠ `accessors` and `bare_call` ARE here, and carry [`detail_row`]'s meaning exactly, because a
/// user file declaring `fn outputs()` has the same two-families-of-name shape a multi-output
/// built-in does. Without them a consumer reading `name` would type a call that raises on every
/// bar. `outputs` stays absent because this row already determines it: the accessor lines ARE the
/// output lines, and an empty `accessors` means the one line is the file's own name.
pub(crate) fn user_detail_row(r: &UserRow) -> Value {
    let params: Vec<Value> =
        r.params.iter().map(|(n, d)| json!({ "name": n, "default": d })).collect();
    let accessors: Vec<Value> =
        r.accessors.iter().map(|(line, call)| json!({ "line": line, "call": call })).collect();
    json!({
        "name": r.name,
        "params": params,
        "accessors": accessors,
        "bare_call": r.bare_call,
        "source": "user_data/indicators",
    })
}

/// The category name a row reports — the `vike_indicators::Category` variant, which is also what
/// `--category` and the MCP tool's `category` argument match against. ONE spelling, so the filter
/// argument is always a value the listing itself printed.
pub(crate) fn category_name(m: &IndicatorMeta) -> String {
    format!("{:?}", m.category)
}

/// One indicator in FULL, as the machine row shared by `--json` and the MCP tool.
///
/// `params` and `outputs` are the indicator's OWN registry surface;
/// `crates/vike-script/src/engine/builtin.rs`'s `register_indicators` is the authority for the call shape
/// built over them (today: one form per argument count, from zero up to the parameter count, the
/// omitted ones taking the defaults printed here). Without them a caller choosing a two-parameter
/// indicator cannot know it HAS a second parameter, let alone what leaving it out means — which is
/// the class of silent wrongness this tool's narrowing was introduced to prevent.
pub(crate) fn detail_row(m: &IndicatorMeta) -> Value {
    let params: Vec<Value> = m
        .params
        .iter()
        .map(|p| json!({ "name": p.name, "default": p.default, "min": p.min, "max": p.max }))
        .collect();
    let outputs: Vec<&str> = m.outputs.iter().map(|o| o.name).collect();
    // The spelling a script actually types for each line. Absent for a single-output indicator,
    // whose bare name IS its one line; present for every multi-output one, where the bare name is
    // usually refused and the accessor is the only way in.
    let accessors: Vec<Value> = vike_script::line_accessors(m.name)
        .into_iter()
        .map(|(line, call)| json!({ "line": line, "call": call }))
        .collect();
    json!({
        "name": m.name,
        "pretty": m.pretty,
        "category": category_name(m),
        "params": params,
        "outputs": outputs,
        "accessors": accessors,
        // Whether the BARE name resolves. `false` with a non-empty `accessors` is the band-indicator
        // shape: reachable, but only line by line. Without this flag a consumer reading `outputs`
        // would have no way to tell `bollinger(20)` from `macd(12, 26, 9)`.
        "bare_call": vike_script::RHAI_INDICATORS.contains(&m.name),
    })
}

/// One indicator as the COMPACT row: the two fields a roster needs to be navigable. See
/// `crates/vike-cli/src/cmd/mcp/offline_tools.rs`'s `tool_list_indicators` for why the default MCP response is
/// this and not [`detail_row`].
pub(crate) fn compact_row(m: &IndicatorMeta) -> Value {
    json!({ "name": m.name, "category": category_name(m) })
}

/// Narrow `rows` to one category, or explain which ones exist.
///
/// An unknown category is an ERROR that names the alternatives rather than an empty listing: an
/// empty answer to `--category momentumm` is indistinguishable from "this build binds nothing in
/// that family", and the second reading is the one that sends somebody hunting through a config.
pub(crate) fn filter_by_category(
    rows: &[&'static IndicatorMeta],
    category: &str,
) -> Result<Vec<&'static IndicatorMeta>, String> {
    let wanted = category.trim();
    let hit: Vec<&'static IndicatorMeta> =
        rows.iter().copied().filter(|m| category_name(m).eq_ignore_ascii_case(wanted)).collect();
    if hit.is_empty() {
        let mut known: Vec<String> = rows.iter().map(|m| category_name(m)).collect();
        known.sort_unstable();
        known.dedup();
        return Err(format!("no category '{wanted}' — this build has: {}", known.join(", ")));
    }
    Ok(hit)
}

/// One bound indicator by exact name, or an error that separates the two ways a name can miss.
///
/// "Not in the registry at all" and "a real indicator this host does not bind" are different
/// mistakes with different repairs, and collapsing them into one message is how somebody spends an
/// afternoon on a typo.
pub(crate) fn find_bound(name: &str) -> Result<&'static IndicatorMeta, String> {
    let name = name.trim();
    if let Some(m) = bound_metas().into_iter().find(|m| m.name == name) {
        return Ok(m);
    }
    Err(match vike_indicators::get(name) {
        // The host's OWN reason, never a paraphrase: `vike_script::unbound_reason` is exported for
        // exactly this, so an advertising surface can say WHY a real indicator is missing instead
        // of silently omitting it. The `unwrap_or` arm is unreachable while every exclusion carries
        // a reason, and is a sentence rather than a panic because a listing is not worth aborting.
        Some(_) => {
            let why = vike_script::unbound_reason(name)
                .unwrap_or("this build's Rhai host does not register it");
            format!("'{name}' is a vike-indicators indicator a Rhai script cannot call: {why}")
        }
        None => format!("no indicator named '{name}'"),
    })
}

/// `name(param=default, …)` — the FULL call form, with the indicator's own defaults spelled out.
///
/// Spelling every parameter is what makes the listing worth reading: the host registers one form
/// per argument count, from zero (all defaults) up to the full list, so this row is simultaneously
/// the widest call and the answer to "what do the ones I leave out become".
fn call_form(m: &IndicatorMeta) -> String {
    let params: Vec<String> =
        m.params.iter().map(|p| format!("{}={}", p.name, fmt_num(p.default))).collect();
    format!("{}({})", m.name, params.join(", "))
}

/// A parameter default, without the trailing `.0` that makes a period look like a tolerance.
fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 { format!("{v:.0}") } else { format!("{v}") }
}

/// The grouped rows: one block per category, in the order the registry introduces them (so the
/// curated grouping survives), each row `call_form  pretty  -> line`.
///
/// The output line is NAMED, and only when it differs from the indicator's own name — so a row says
/// which number THE CALL PRINTED IN THAT ROW hands back, on the rows where the call does not already
/// say it. A COUNT would be the wrong column: it would print `1` on most rows and say nothing, and
/// on a multi-output row it would say how many lines exist without naming the one this call returns.
///
/// ⚠ **Not "the bare name" — the call this row prints**, which for some rows is not a bare call at
/// all. [`bound_metas`] deliberately admits an indicator reachable ONLY through a per-line accessor
/// (`bollinger` is the standing example: its bare name is refused because line 0 is the upper band),
/// and `call_form` prints whatever spelling that row actually offers. An earlier draft of this
/// paragraph said the column "describes the BARE name only", which was false for exactly the rows
/// where the distinction matters.
///
/// A multi-output indicator's OTHER lines are reached through `vike_script::line_accessors`, which
/// `--json` carries per row as `accessors` and this block does not print — the human roster is a map
/// of the catalog, and one row per line would treble it.
///
/// PURE (returns the text rather than printing it) so the tests read what a user reads.
fn render_rows(rows: &[&'static IndicatorMeta]) -> String {
    let width = rows.iter().map(|m| call_form(m).len()).max().unwrap_or(0).min(40);
    let label = rows.iter().map(|m| m.pretty.len()).max().unwrap_or(0).min(30);
    // Categories in FIRST-APPEARANCE order, each block complete. Emitting a header whenever the
    // category CHANGES would be one line shorter and wrong: the registry is ordered by source file,
    // not by category, so an interleaved category would print its header several times and read as
    // several families with the same name.
    let mut order: Vec<String> = Vec::new();
    for m in rows {
        let cat = category_name(m);
        if !order.contains(&cat) {
            order.push(cat);
        }
    }
    let mut out = String::new();
    for (i, cat) in order.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!("{cat}\n"));
        for m in rows.iter().filter(|m| &category_name(m) == cat) {
            let line = m.outputs.first().map(|o| o.name).unwrap_or("value");
            let returns = if line == m.name { String::new() } else { format!("  -> {line}") };
            let row = format!("  {:<width$}  {:<label$}{returns}", call_form(m), m.pretty);
            out.push_str(row.trim_end());
            out.push('\n');
        }
    }
    out
}

/// [`render_rows`], the user's own block, and the footer. The counts are COMPUTED from what was
/// printed — the only numbers this feature states, and they are stated nowhere in prose.
///
/// With no user indicators installed the output is byte-identical to what it always was: no block,
/// and the same one-number footer. The split is stated only when there is a split to state — a
/// footer reading "…(N built in, 0 of your own)" on every fresh install would be noise about
/// nothing, and it is the ONE line that would then differ between two boxes running one build.
fn render(rows: &[&'static IndicatorMeta], users: &[UserRow]) -> String {
    let user_block = render_user_rows(users);
    let total = rows.len() + users.len();
    let footer = if users.is_empty() {
        format!("{total} callable in this build.\n")
    } else {
        format!(
            "{total} callable in this build ({} built in, {} of your own).\n",
            rows.len(),
            users.len()
        )
    };
    if user_block.is_empty() {
        format!("{}\n{footer}", render_rows(rows))
    } else {
        format!("{}\n{user_block}\n{footer}", render_rows(rows))
    }
}

/// Run the subcommand. Takes no dispatcher state and opens no file: the built-in roster depends on
/// the BUILD alone, and the user's own indicators are read out of PROCESS state the dispatcher
/// already installed — never a credential, and never a directory this function resolves.
///
/// ⚠ That second half is why the answer is no longer a pure function of the build: two boxes
/// running one binary print different listings when one of them has written an indicator. It is
/// the point, and it is also why `crates/vike-cli/tests/indicators_cli.rs` pins
/// `VIKE_USER_DATA_DIR` at an empty directory before asserting anything about the built-in set.
pub fn run(args: impl Iterator<Item = String>) -> ExitCode {
    let args = match parse(args) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("indicators", USAGE, &msg),
    };
    let all = bound_metas();
    let users = user_rows();
    // `--name` answers about ONE indicator and, when it is held back, says why — so it exits
    // NON-ZERO in that case. "Not callable" is the answer a script author acts on, and an exit 0
    // would make it indistinguishable from a successful lookup to anything scripting this command.
    //
    // ⚠ The user's OWN indicators are searched FIRST. `find_bound` answers "no indicator named
    // 'my_ema'" for a name it does not know, and that sentence about a file on the author's own
    // disk is not a miss — it is wrong. Searching them first also makes the answer match what a
    // SCRIPT would get: `crates/vike-script/src/engine/host.rs`'s `build_engine` registers the installed
    // set over the built-ins, and `user_indicator_conflict` already refuses a shadowing name at
    // load, so the two orders can never actually disagree — this one is simply the one that stays
    // right if that refusal is ever relaxed.
    if let Some(name) = &args.name {
        if let Some(u) = users.iter().find(|u| u.name == name.trim()) {
            print_listing(&[], std::slice::from_ref(u), args.json);
            return ExitCode::SUCCESS;
        }
        return match find_bound(name) {
            Ok(m) => {
                print_listing(&[m], &[], args.json);
                ExitCode::SUCCESS
            }
            Err(msg) => {
                eprintln!("vike-cli indicators: {msg}");
                ExitCode::FAILURE
            }
        };
    }
    // ⚠ `--category` drops the user block entirely, and that is the honest answer rather than a
    // gap: a user indicator carries no category, so no category can contain one. Printing them
    // under a filtered listing would put rows in a family they are not in.
    let (rows, users) = match &args.category {
        Some(c) => match filter_by_category(&all, c) {
            Ok(r) => (r, Vec::new()),
            Err(msg) => {
                eprintln!("vike-cli indicators: {msg}");
                return ExitCode::FAILURE;
            }
        },
        None => (all, users),
    };
    if args.json {
        print_listing(&rows, &users, true);
    } else {
        print!("{}", render(&rows, &users));
    }
    ExitCode::SUCCESS
}

/// Print a listing in whichever shape was asked for. The human form here carries NO footer: a count
/// is what a roster owes its reader, and "1 callable in this build" under a single-name lookup would
/// read as a claim about the build.
///
/// The JSON form ALWAYS emits both keys, including as empty arrays. A consumer that has to test for
/// a key's presence before reading it is one `--name my_ema` away from a crash, and an absent
/// `user_indicators` is indistinguishable from a build that does not support them at all.
fn print_listing(rows: &[&'static IndicatorMeta], users: &[UserRow], as_json: bool) {
    if as_json {
        let payload: Vec<Value> = rows.iter().map(|m| detail_row(m)).collect();
        let user_payload: Vec<Value> = users.iter().map(user_detail_row).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({ "indicators": payload, "user_indicators": user_payload })
            )
            .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
        );
    } else {
        print!("{}{}", render_rows(rows), render_user_rows(users));
    }
}

#[path = "indicators_tests.rs"]
#[cfg(test)]
mod indicators_tests;
