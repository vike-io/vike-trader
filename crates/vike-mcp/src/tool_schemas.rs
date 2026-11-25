//! The `tools/list` roster: `tools_spec`, every tool this server implements with its description and
//! risk annotations, its `inputSchema` generated from the struct `crates/vike-mcp/src/tool_args.rs`
//! holds for it.
//!
//! ⚠ **An `outputSchema` only where every SUCCESS answer is one struct** (`Row::answers`), the struct the
//! handler builds that answer from. rmcp checks nothing, while a client on the TypeScript SDK (Claude
//! Code, Claude Desktop) FAILS a call whose `structuredContent` does not match, so a tool answering two
//! shapes (a preview or a receipt, a run or its background id) declares none, and the text `content` stays
//! beside it for every client. `crates/vike-mcp/tests/shipped_mcp/mcp_wire_snapshot/output_schemas.rs`
//! holds every success answer in the goldens to its tool's schema, and `crate::protocol`'s `tool_response`
//! every in-process test's.
//!
//! The router (`Server::call_tool`), the scoping rings (`ToolAccess`) and the registry-manifest gate
//! all read this ONE function. `scripts/gen_skills.sh` reads what it serves from the committed
//! `crates/vike-mcp/tests/fixtures/tool_facts.tsv`, which `crates/vike-mcp/src/tests/tool_facts.rs`
//! holds equal to it.

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::background_runs::{MAX_ENDED, MAX_RUNNING, RunStatusAnswer};
use super::backtest_tools::ListStrategiesAnswer;
use super::data_tools::ListSeriesAnswer;
use super::is_write_tool;
use super::offline_tools::{
    DiscoverParamsAnswer, ListIndicatorsAnswer, ListTemplatesAnswer, ValidateStrategyAnswer,
};
use super::tool_args::*;

/// One tool on the roster.
pub(super) struct Row {
    pub(super) spec: Value,
    /// It dials a node only to ADD to an answer its arguments alone give, so the `offline` ring serves it
    /// reading nothing and advertises it there with `openWorldHint: false` (`crate::scope::ToolAccess`).
    /// The tool's own declaration, like the hint beside it: no list of names says it.
    pub(super) offline_too: bool,
    /// The arguments `T` declares, in the order it declares them, the gate's left out.
    #[cfg(test)]
    pub(super) declared: &'static [&'static str],
}

/// A tool's `spec` with the `inputSchema` of `T`, and the gate's arguments when it is a write tool.
/// [`is_write_tool`] decides, so a tool cannot be on the write roster and miss them.
///
/// The spec's `title` is the display name a client shows; it is copied into `annotations.title`, where a
/// client of the 2025-03-26 revision (the one before `title`) reads it.
fn row<T: schemars::JsonSchema + DeserializeOwned>(mut spec: Value) -> Row {
    spec["annotations"]["title"] = spec["title"].clone();
    let mut input = input_schema::<T>();
    if is_write_tool(spec["name"].as_str().unwrap_or_default())
        && let (Some(all), Value::Object(gate)) =
            (input["properties"].as_object_mut(), input_schema::<WriteGate>()["properties"].take())
    {
        all.extend(gate);
    }
    spec["inputSchema"] = input;
    Row {
        spec,
        offline_too: false,
        // The field names in declaration order, which a schema cannot give back (`tool_args.rs`'s
        // module doc).
        #[cfg(test)]
        declared: vike_utility::serde_fields::declared_fields::<T>(),
    }
}

impl Row {
    /// This tool's `outputSchema`: its answer struct `T`'s (the module doc says which tools have one).
    fn answers<T: schemars::JsonSchema>(mut self) -> Row {
        self.spec["outputSchema"] = output_schema::<T>();
        self
    }
}

/// Every tool this server IMPLEMENTS — each one's schema + risk annotations.
///
/// ⚠ This is the FULL roster, not the answer to `tools/list`: a session serves
/// [`super::ToolAccess::advertised`], which is this filtered by the profile. It stays whole
/// deliberately — `server.json`'s registry listing and the `skills/` package describe the SERVER,
/// not one launch of it, and both gates read this function.
pub(super) fn tools_spec() -> Value {
    Value::Array(roster().into_iter().map(|tool| tool.spec).collect())
}

/// [`tools_spec`], row by row.
pub(super) fn roster() -> Vec<Row> {
    vec![
        row::<ValidateStrategyArgs>(json!({
            "name": "validate_strategy",
            "title": "Check a strategy",
            "description": "Compile a Rhai strategy; returns {ok, error?}. Compile IS validation. Offline — no server.",
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        }))
        .answers::<ValidateStrategyAnswer>(),
        row::<DiscoverParamsArgs>(json!({
            "name": "discover_params",
            "title": "Show a strategy's parameters",
            // ⚠ It reads as an INSPECTION tool for somebody else's script, and that is how the
            // first agent-eval model run read it: having authored the strategy itself, the agent
            // answered "which parameters can I tune" from its own memory of what it had just
            // written and never called this. It happened to be right and had no way to know it —
            // `validate_strategy` answers `{ok: true}` and says nothing about params, and this tool
            // runs the TOP LEVEL only, so the one mistake worth catching (a `param()` inside a
            // hook, which nothing reports) is invisible to the author too. So the text now says what
            // the tool is FOR and what it cannot see. `crates/vike-agent-evaluation/src/evaluation/cases.rs`'s
            // `WRITE_A_RHAI_STRATEGY` is the measurement.
            //
            // ⚠ That rewrite OVERSHOT on one clause: it said a `param()` inside `on_bar()` was
            // "undrivable by a sweep", which made the tool read as a correctness gate over what a
            // grid can reach. It is false, and both halves were read back off the code before this
            // was reworded. `crates/vike-rhai/src/engine/host.rs`'s `param` registration reads
            // `overrides` on EVERY call rather than only during the one-time top-level run, and
            // `crates/vike-backtest/src/run/strategy_builder.rs`'s `rhai_overrides` forwards every
            // numeric param key except `src` with no comparison against the declared set — so a
            // grid key naming a hook-buried knob reaches it and drives it. The real limitation is
            // narrower and duller: discovery never REPORTS such a knob, so nothing tells the author
            // (or whoever writes the grid) that it is there or what its default is. Claiming more
            // than that teaches an agent to conclude a working grid is inert.
            "description": "Read back the tunable param(name, default) knobs a Rhai strategy declares — the exact keys a profile's [strategy.params] or a [paramscan] grid may override, in declaration order. Call it on a script you WROTE as well as one you were handed: it runs the script's TOP LEVEL only, so a param() inside on_bar() is NOT REPORTED here (a sweep can still drive such a knob by name — but nothing tells you it is there), and compiling clean says nothing about it. Offline — no server.",
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        }))
        .answers::<DiscoverParamsAnswer>(),
        row::<NoArgs>(json!({
            "name": "list_templates",
            "title": "Show starter strategies",
            "description": "List starter Rhai strategies as {name, code} — each parameterized via param() so it drops straight into run_sweep. Offline — no server.",
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        }))
        .answers::<ListTemplatesAnswer>(),
        row::<ListIndicatorsArgs>(json!({
            "name": "list_indicators",
            "title": "Show indicators",
            // ⚠ This text ships in EVERY session's tools/list, called or not, so it names none of
            // the roster — see `list_indicators_description_does_not_enumerate_the_set`. It used
            // to splice `RHAI_INDICATORS.join(", ")` in, which was 13 characters when the host
            // bound three names and is kilobytes now that it binds the catalog.
            "description": "List the HOST-BOUND indicators a Rhai strategy can actually call — what the vike-rhai host registers, which is not every vike-indicators registry name; a script calling an unbound one compiles and then fails on every bar. Your own (user_data/indicators) come under `user_indicators`. No arguments returns the compact roster (name + category). Pass `category` for one family in full, or `name` for one indicator in full — its parameters with defaults and its output line — which also answers WHY a registry name is not callable. The roster is not repeated in this text on purpose: it is long, and a description is sent whether you call the tool or not.",
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        }))
        .answers::<ListIndicatorsAnswer>(),
        row::<ProfileTemplateArgs>(json!({
            "name": "profile_template",
            "title": "Show backtest settings",
            "description": "What a backtest profile TOML may say, read from the published profile schema this build's backtest daemon parses (the release's profile.json). With no argument: every table, \"\" the top level, with its key names. With `table`: each of its keys in full — type, required, default, the words it takes and what it means. Write run_backtest's, run_sweep's and run_walk_forward's `profile` from it. Offline — no server.",
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        })),
        row::<RunBacktestArgs>(json!({
            "name": "run_backtest",
            "title": "Run a backtest",
            "description": "Run a backtest on the remote backtest daemon (`vike-backend backtesthub --addr`, dialled at --backtest-addr) from a profile TOML whose [strategy] file names a strategy file the daemon holds (list_strategies); an inline `script` is no longer supported and is refused. Returns the TradingResults JSON; a `strategy_failures` key in it means the strategy failed during the run: say so first, the result is not trustworthy. ONE point only: a profile with a non-empty [paramscan] table (or the legacy [paramscan] spelling) is a parameter search and the daemon REFUSES it here — send it to run_sweep. Cancellable: notifications/cancelled drops the response and closes the daemon connection, and the daemon stops loading the run's series once it sees the connection gone (a run already inside the engine finishes first). With background: true it answers a run_id at once and runs on: run_status reads it, cancel_run stops it.",
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        })),
        row::<RunSweepArgs>(json!({
            "name": "run_sweep",
            "title": "Search parameters",
            "description": "Run a parameter search on the remote backtest daemon (--backtest-addr) from a profile TOML with a [paramscan] table (each key overrides strategy.params.<key> across a value grid; the legacy [paramscan] spelling still loads), its [strategy] file naming a strategy file the daemon holds (an inline `script` is refused). DEFAULT is the exhaustive grid; `optimizer` selects a smarter search (euler = successive halving, tpe = Bayesian, genetic) and each method takes its own knob — a knob under a method that does not own it is REFUSED by name, never discarded. Returns the server-ranked search, one row per grid point, best first: each row's overrides, its report (every metric-catalog number, keyed by metric id: sharpe, total_return, max_drawdown, n_trades, profit_factor, ...), error and score, the rows paged in for you; no trades and no curves, which stay in the run the daemon saved (saved_run; point N is that run's report.json rows[N-1]). A top-level `strategy_failures` (every point's together) means the strategy failed during the search: say so first, the ranking is not trustworthy. An older daemon answers the whole ParamscanReport (each row's whole TradingResults) instead. Cancellable: notifications/cancelled drops the response and closes the daemon connection, and the daemon stops evaluating grid points once it sees the connection gone. With background: true it answers a run_id at once and runs on: run_status reads it, cancel_run stops it.",
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        })),
        row::<RunWalkForwardArgs>(json!({
            "name": "run_walk_forward",
            "title": "Walk-forward test",
            "description": "Run an out-of-sample walk-forward on the remote backtest daemon (--backtest-addr) from a profile TOML with a [walkforward] table, its [strategy] file naming a strategy file the daemon holds (an inline `script` is refused). DEFAULT (n_splits alone) is the FIXED-parameter stability walk: every window trades the profile's own [strategy.params] and chooses nothing — do NOT report such a run as an optimization. search = \"paramscan\" in [walkforward] runs the other protocol: each window re-scores the [paramscan] grid on its OWN training half and trades only that window's winner (mode = anchored|rolling, rank_by = sharpe|return|max_dd|equity); it needs a [paramscan] table and is refused without one. Returns the stitched walk-forward results JSON (steps, each with its own out-of-sample results, + the overall results of the stitched curve + profitable_step_share; the curve itself, `equity_curve`, only with include_curve: true) — only a step that searched carries chosen_params, which is how you tell the two runs apart. A `strategy_failures` key in overall (or a step's results) means the strategy failed there: say so first, those numbers are not trustworthy. Run both on one profile: the no-search control is the only evidence that searching bought anything. Cancellable: notifications/cancelled drops the response and closes the daemon connection, and the daemon stops starting windows (and, under search = \"paramscan\", training-half grid points) once it sees the connection gone. With background: true it answers a run_id at once and runs on: run_status reads it, cancel_run stops it.",
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        })),
        row::<RunIdArgs>(json!({
            "name": "run_status",
            "title": "Check a background run",
            "description": format!("Report ONE background run (a run_backtest, run_sweep or run_walk_forward called with background: true) by its run_id: `state` (running | finished | failed | cancelled) and `elapsed_ms` (the daemon sends no finer progress), and once it ended `answer`, exactly what the tool would have answered in the foreground, or `error`, its refusal or failure in the tool's words. Call it again while `state` is running. A run lives in THIS server process: it stops when the process ends, an id from another process is unknown here, at most {MAX_RUNNING} run at once, and only the newest {MAX_ENDED} ended runs are kept."),
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        }))
        .answers::<RunStatusAnswer>(),
        row::<RunIdArgs>(json!({
            "name": "cancel_run",
            "title": "Stop a background run",
            "description": "Stop ONE running background run by its run_id: its daemon connection is closed, as notifications/cancelled closes a foreground run's, and the daemon stops at the run's next unit of work once it sees the connection gone. Answers the run as run_status does, `state: cancelled`, with no answer: the work done is thrown away. A run that already ended is refused, nothing is left to stop.",
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        }))
        .answers::<RunStatusAnswer>(),
        row::<NoArgs>(json!({
            "name": "list_strategies",
            "title": "Show available strategies",
            "description": "List the strategy files the remote backtest daemon's strategies/ folder holds, each by its path under that folder: the value a profile's [strategy] file names. Requires a reachable backtest daemon (--backtest-addr).",
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
        }))
        .answers::<ListStrategiesAnswer>(),
        row::<NoArgs>(json!({
            "name": "list_series",
            "title": "Show stored data",
            "description": "List the stored market data series the remote vike-datahub server holds, with coverage: {kind, venue, symbol, group, interval, first_ts, last_ts, rows} — what a profile's [data] table can name. Account series (your own fills, orders, equity) are not listed: `withheld_account_series` counts them. At most 150 rows: past that the answer carries `truncated`, `total` and a `note` naming the command that lists them all. Requires a reachable datahub (--addr).",
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
        }))
        .answers::<ListSeriesAnswer>(),
        row::<NoArgs>(json!({
            "name": "node_snapshot",
            "title": "Show orders and positions",
            "description": "Read the running vike-tradehub's live state (orders, positions, per-venue equity, recent events). An order the node HOLDS until a moment (submit_order's release_at) is not in `orders` yet: it is in `held_exits` with its `release`, and cancel_order cancels it by its order_client_id. The frame's `bars` are left out, so the answer stays small enough for a client to show. ⚠ Top-level `positions`, `balance` and `trading_state` are the PRIMARY engine's alone and `equity_total` covers only its currency group, so on a node with several accounts read `venues[].positions` before calling anything flat. Requires --node and this box's device key on the node's access list (role observe or higher). If the result carries `pre_fold: true`, the frame holds nothing built: with `identity` present it is the placeholder the node publishes before its first fold; with `identity: null` it is this client's own placeholder, because no node frame arrived in time. Either way venues, balance, equity_total and accounts_epoch are placeholders, `venues: []` does NOT mean nothing is mounted, and `pre_fold_note` says which — call it again. If the node connection has dropped, this returns an ERROR naming it DOWN and the last frame STALE, never that frame as if live, and reconnects on the next call, so call it again rather than restarting anything. ⚠ A link that dies SILENTLY (a sleeping laptop, an ssh tunnel without ServerAliveInterval) is noticed only once the node's heartbeat has been missing for about 45 s, and never on a node too old to send one: until then the last frame is answered as live, and the frame carries no timestamp to age it by. If the picture never changes while the node should be trading, have the operator check the tunnel before trusting it.",
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        })),
        row::<NoArgs>(json!({
            "name": "strategy_status",
            "title": "Show running strategies",
            // ⚠ The row's fields are named because an agent acts on two of them: `mount_id` is what it hands
            // `unmount_strategy`, and `tier` is what tells a demo account from a live one, which `live` does
            // not. `MountViewWire`'s docs are the authority on each field.
            "description": "Ask the running vike-tradehub WHAT IT IS RUNNING: which daemon answered (name, live/paper, build), the daemon's resolved effective-params line, and one row per strategy the node runs NOW, a mount added while it runs included and an unmounted one gone. A row carries the strategy name, that mount's params, its `mount_id` (the id unmount_strategy takes), `venue`/`symbol`/`interval`, `typed_params`, `route_key` (the account it trades), `tier` (the tier that account runs at now: paper, demo or live), `not_running` (present when the node does not run that account, so its orders are refused) and `live` (its orders reach a venue, so a demo account reads true: read `tier`; it may disagree with the daemon's own live flag). A field a node predates is absent from its rows. Read-only, over the OBSERVE scope: it can neither place nor change anything. Requires --node and this box's device key on the node's access list (role observe or higher) and a node that advertises the strategy verbs; an older node is refused CLIENT-SIDE with nothing sent.",
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
        })),
        row::<NoArgs>(json!({
            "name": "settings_show",
            "title": "Show node settings",
            // ⚠ NO subject word from `the_mcp_surface_advertises_no_credential_writer`'s SUBJECT
            // list may appear here, and the temptation is real: the natural sentence for the
            // redaction clause reaches for the word this surface may not pair with an act word.
            // "sensitive-looking" carries the same fact and trips nothing — and the redaction is
            // the NODE's anyway (`SettingsShowRowWire`'s doc: rows arrive already redacted, on
            // construction, so no serializer on this side could leak one).
            "description": "Read the running vike-tradehub's EFFECTIVE settings: `settings_dir`, the settings directory it resolved at boot (null = compiled-in defaults), then `rows`, one per typed key: its `section` (the key's first segment), its full dotted `key`, the effective `value` (empty = unset), its `origin`, the layer that set it (`default` or `db`), and `read_by`, what actually READS it. ⚠ A `refusal` beside them means the node could not read its settings database or does not trust it: every row may then be a compiled-in default reading `origin: default`, a ceiling as absent, so report the refusal and none of the rows. Sensitive-looking values arrive already redacted by the node. Read-only, over the OBSERVE scope. Requires --node and this box's device key on the node's access list (role observe or higher) and a node that advertises the settings-show capability; an older node is refused CLIENT-SIDE with nothing sent. `rows[].key` is the exact spelling set_setting takes.",
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
        })),
        row::<NoArgs>(json!({
            "name": "health_node",
            "title": "Check node health",
            "description": "Judge the running vike-tradehub in one call: the `vike-cli trade health --json` document. `verdict` (OK | WARN | FAIL), the `warn`, `fail` and `unknown` counts, and `checks[]`, one row per finding as {status, check, account, detail, items}: is its door ready, which accounts run and at which tier or are NOT RUNNING and why, halted or reducing venues, held reconcile rows, resting orders, positions with no price, the HALT sentinel. An UNKNOWN row is a fact the wire does not carry and names the command that answers it; it does not move the verdict. Plus `gatewayhub`, that daemon as the datahub (--addr) sees it: {via, reached, error, uptime_ms, epoch, connections, stream_sessions, fetches_running, market_data_venues, credentialed_data_venues}; it says nothing of any venue. ⚠ A dead tradehub is a RESULT here, not an error: `verdict: FAIL` and a row saying it does not answer, so read `verdict`. Read-only, over the OBSERVE scope: no command, no event. Requires --node and this box's device key on the node's access list (role observe or higher).",
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
        })),
        row::<ReportArgs>(json!({
            "name": "report",
            "title": "Show the live trade report",
            "description": "Read ONE live account's trading results: its own fills out of the datahub (--addr) over the window, its tier and its ledgers from the vike-tradehub's snapshot, folded into the realized round-trips and their metrics, the same document `vike-cli report account:<venue>/<LABEL> --json` prints. `name` heads it with the account, its tier and its period, then says what was counted or left out, one sentence each: the starting capital and where it came from, FOREIGN fills (placed outside this install, on the account all the same, so they count), fills recorded before the execution stream (they name no account, so they are left out). The curve is the REALIZED one, from `cash`; `cash` and `periods_per_year` rescale the ratios. A refusal is an ERROR here, never an empty report: an account the node names nowhere or does not run (its ledger and contracts are then unknown), a symbol its kept row or its name quotes off the dollar or in no known currency, a datahub that records no live executions, an opening position nothing can read. It reads no finished backtest: that report is what run_backtest returned. Read-only. Requires --node and this box's device key on the node's access list (role observe or higher), and a datahub that records live executions.",
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
        })),
        // Served by `offline` too, reading no node there: it decodes from the id alone.
        Row {
            offline_too: true,
            ..row::<DecodeOrderIdArgs>(json!({
                "name": "decode_order_id",
                "title": "Explain an order ID",
                "description": "Say what an order client id means: who placed it (G GUI, C CLI, T Telegram, M this server, S a strategy, E the engine), the strategy and account codes, whether it opens or closes and which way, its group and role (entry, stop, take-profit) and its replacement number, as JSON with one plain-words line (`words`). Works with no node; with --node and this box's device key on the node's access list (role observe or higher), outside --profile offline, it adds the names the node gives: the account's label, the strategy behind the code, and the group's other orders resting now. `node` says where the names came from: read, unreachable (`node_error` says why, and the names are null), none (no --node) or offline (this profile reads no node). `form` is v2, v1 (an id minted before these codes) or other (a venue's own id, a foreign order's, one typed by hand). Labels decide nothing. Read-only.",
                "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
            }))
        },
        row::<SubmitOrderArgs>(json!({
            "name": "submit_order",
            "title": "Place an order",
            "description": "Submit an order on the vike-tradehub. WITHOUT confirm:true this only PREVIEWS (executes nothing). Requires --node and this box's device key on the node's access list (role control or higher).",
            "annotations": { "destructiveHint": true, "idempotentHint": false, "openWorldHint": true }
        })),
        row::<SubmitOrderListArgs>(json!({
            "name": "submit_order_list",
            "title": "Place linked orders",
            "description": "Submit ONE order list on the vike-tradehub: an oco pair, an oto entry with its exit, or a ladder (an entry, one stop for its whole size and partial targets), every leg on the one account and symbol named and every leg with its own order client id. The node takes it only on an engine that already trades the symbol, and a venue that cannot hold a leg refuses the WHOLE list. WITHOUT confirm:true this only PREVIEWS (executes nothing). Requires --node and this box's device key on the node's access list (role control or higher).",
            "annotations": { "destructiveHint": true, "idempotentHint": false, "openWorldHint": true }
        })),
        row::<SubmitComboArgs>(json!({
            "name": "submit_combo",
            "title": "Place a combo order",
            "description": "Submit ONE combo on the vike-tradehub: two legs or more traded as ONE order on the one account named (omit `account` only where the node has one account of the venue, running or not), priced by its signed net_limit, by a limit on every leg, or at market. Only deribit's row places a combo, by its net, and only a paper book fills one priced by its legs: any other engine rejects it after the node's answer, by name, on the order. The combo has ONE order client id (order_client_id, or one the server mints and the preview shows): a resend under it is refused whole, never placed twice. A node without the combo capability is refused CLIENT-SIDE with nothing sent. WITHOUT confirm:true this only PREVIEWS (executes nothing). Requires --node and this box's device key on the node's access list (role control or higher).",
            "annotations": { "destructiveHint": true, "idempotentHint": false, "openWorldHint": true }
        })),
        row::<CancelOrderArgs>(json!({
            "name": "cancel_order",
            "title": "Cancel an order",
            "description": "Cancel one resting order by order_client_id. WITHOUT confirm:true this only PREVIEWS.",
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        })),
        row::<ModifyOrderArgs>(json!({
            "name": "modify_order",
            "title": "Change an order",
            "description": "Modify one resting order's qty, price and/or trigger price by order_client_id (at least one of new_qty/new_price/new_trigger_price). PREVIEW IS MANDATORY: WITHOUT confirm:true this only PREVIEWS (executes nothing).",
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        })),
        row::<FlattenArgs>(json!({
            "name": "flatten",
            "title": "Close a position",
            "description": "Close ONE account's (venue, symbol) net position with a reduce-only market order. Omit `account` only where the node has one account of the venue, running or not; anywhere else the call is REFUSED with nothing sent, listing the accounts. WITHOUT confirm:true this only PREVIEWS.",
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        })),
        row::<CloseAllArgs>(json!({
            "name": "close_all",
            "title": "Cancel all and close all",
            "description": "PANIC BUTTON — cancel every live order then flatten every position, on the WHOLE NODE when `venue` is omitted. With `venue` it reaches ONE account: omit `account` only where the node has one account of the venue, running or not; anywhere else the call is REFUSED with nothing sent, listing the accounts. WITHOUT confirm:true this only PREVIEWS.",
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        })),
        row::<MassCancelArgs>(json!({
            "name": "mass_cancel",
            "title": "Cancel all orders",
            "description": "Cancel EVERY live order, on the WHOLE NODE when `venue` is omitted, optionally for one symbol. With `venue` it reaches ONE account: omit `account` only where the node has one account of the venue, running or not; anywhere else the call is REFUSED with nothing sent, listing the accounts. PREVIEW IS MANDATORY: WITHOUT confirm:true this only PREVIEWS (executes nothing).",
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        })),
        row::<SetTradingStateArgs>(json!({
            "name": "set_trading_state",
            "title": "Pause or halt trading",
            // ⚠ The second sentence is here because an agent REACHED FOR THIS TOOL to "switch
            // binance to live". The first agent-eval model run refused correctly — no write, no key
            // echoed — and then told the operator the remedy was "the node's config/env and a
            // restart", which cannot work: no environment variable arms a venue, and the operator
            // who follows that edits something nothing reads, and nothing changes, with no error. This
            // is the only tool on the roster whose name reads like "go live", and it now says what
            // it is not and where the decision actually lives.
            //
            // ⚠ It names the ACCOUNT-ROW half and stops there, deliberately, and it spells the
            // verbs without their `secrets` parent because that word is a credential SUBJECT for
            // the scan below. The credential half is fenced by `docs/decisions/0036…` and by
            // `the_mcp_surface_advertises_no_credential_writer` below: this description may not
            // pair a credential word with a write word, and it opens with "Set". The human-facing
            // verb lives in the `enable_a_venue` prompt and the `arm-a-venue` skill, which the scan
            // deliberately does not walk and which execute nothing.
            "description": "Set the trading state / kill switch of every account on the node — active | reducing | halted. WITHOUT confirm:true this only PREVIEWS. ⚠ It is NOT a venue arming control and cannot start or stop an account or change its tier: what an account runs at is its ACCOUNT ROW in the settings database's account table — its tier (paper | demo | live) and whether it is active. With no row, an inactive row, missing keys or two active tiers that account is NOT RUNNING and its orders are refused; nothing trades on paper in its place. A human changes the row on the box with the vike-cli `account set-tier` / `account activate` / `account deactivate` verbs, the running node follows it within about a second with no restart, and nothing on this server can change it.",
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        })),
        row::<MountStrategyArgs>(json!({
            "name": "mount_strategy",
            "title": "Start a strategy on the node",
            // ⚠ The `strategy` clause says "ON THE NODE" because the obvious agent mistake is to paste
            // the script it just wrote with `validate_strategy` into this argument. That string is
            // then a filename the daemon cannot open, and the refusal arrives from the node rather
            // than from the schema — a whole round trip to learn that this surface has no way to put
            // a file on that box at all.
            "description": "Add ONE strategy mount to the running vike-tradehub's core, LIVE and without a restart. WITHOUT confirm:true this only PREVIEWS (executes nothing). The source is the `strategy` file: its path under the NODE's strategies/ folder (a .rs or .rhai file) — not script source, and not a path on this machine; this server cannot put a file on that box. `venue` must name an engine the node already runs — read it from node_snapshot's venues[].venue. The node validates at its edge with the same refusals a profile load applies; a refusal the core itself raises later (a duplicate mount id, an unknown venue) surfaces in the node's recent events rather than here. Requires a node that advertises the mount verbs; an older one is refused CLIENT-SIDE with nothing sent.",
            "annotations": { "destructiveHint": true, "idempotentHint": false, "openWorldHint": true }
        })),
        row::<UnmountStrategyArgs>(json!({
            "name": "unmount_strategy",
            "title": "Stop a strategy on the node",
            // ⚠ Two clauses an agent cannot get from anywhere else, and both were read off the
            // wire variant's own doc. The positions one is the safety half: "unmount" reads as
            // "stand this down", and what it really leaves behind is an OPEN POSITION with nothing
            // managing it. The mount-id one is the usability half: it says where the id comes from, so
            // an agent reads it off the status row instead of composing one.
            "description": "Remove ONE strategy mount from the running vike-tradehub's core by MOUNT ID. WITHOUT confirm:true this only PREVIEWS. The node CANCELS that mount's attributed live orders before removing it and saves its durable state — but POSITIONS ARE NOT FLATTENED: whatever the strategy left open stays open with nothing managing it, and `flatten` is the verb for that. ⚠ The id is the row's `mount_id` in strategy_status: the explicit `mount_id` the mount was created with, or the derived `{venue}__{symbol}__{interval}`. An unknown id surfaces in the node's recent events rather than as an error here. Requires a node that advertises the mount verbs.",
            "annotations": { "destructiveHint": true, "idempotentHint": false, "openWorldHint": true }
        })),
        row::<SetSettingArgs>(json!({
            "name": "set_setting",
            "title": "Change a node setting",
            // ⚠ Like `set_trading_state` above, this description may name no credential: the
            // SUBJECT×ACT scan in `the_mcp_surface_advertises_no_credential_writer` would refuse
            // the whole surface for pairing one with the word `set` in this tool's own NAME. That
            // is not a wording problem to route around — the fact an agent needs (that nothing
            // here reaches a credential) is stated once, for the whole server, by
            // [`INSTRUCTIONS_NO_CREDENTIAL`], which is the right altitude for it anyway.
            //
            // ⚠ A MEASURED CONSEQUENCE of the argument being called `key`: the agent transcript
            // records its value as redacted. `crate::trace`'s key rule is
            // `vike_config::is_secret_key`, the workspace's ONE authority on a credential-shaped
            // NAME, and that function matches the bare word `KEY` — so `key`, `setting_key` and
            // every other spelling of this concept is caught. It is not fixable from here and must
            // not be: a second redaction table is precisely what that module refuses to spell.
            // What survives in the record is the tool, the verdict and the `value`, unless the key
            // names a credential ([`crate::trace`]'s third position); what is lost
            // is WHICH key a REFUSED attempt named (the tool's `file` argument, which used to carry
            // the section, went with the file era). The accepted case is unaffected — the node's
            // own audit trail records the write with its old and new values, which is where an
            // executed change is recorded anyway.
            // `the_transcript_redacts_the_settings_key_and_keeps_the_rest` pins both halves, so
            // this stays a known bound rather than a surprise.
            "description": "Write ONE setting on the running node: one row of its settings database, named by its full dotted `key` (policy.* | config.* | preferences.* | flags.*) and validated by the NODE's own loader before it commits — so a write can never leave a store the next boot refuses. WITHOUT confirm:true AND this call's preview_token it only PREVIEWS, and the preview's `change` shows the key's current value on the node beside the new one (old → new). ⚠ This changes a LIVE setting on the node, and a policy.* key is a risk ceiling or an arming switch: confirm one only after the owner said yes in chat. A session started with --unattended refuses every key; settings are then changed by the operator, with `vike-cli config set` or the GUI. The answer's `restart_required` says which happened: false = the node applied the value LIVE, true = it applies at the next restart (every policy.* key — policy is never hot). Read settings_show first: `rows[].key` is the exact spelling this takes. Requires a node that advertises the settings-write capability; an older one is refused CLIENT-SIDE with nothing sent.",
            "annotations": { "destructiveHint": true, "idempotentHint": false, "openWorldHint": true }
        })),
        row::<DeleteSeriesArgs>(json!({
            "name": "delete_series",
            "title": "Delete stored data",
            // ⚠ The description carries the ASYMMETRY with the CLI, because nothing else can: an
            // agent reading `vike-cli data history rm`'s help would learn that `--produced-by` is optional
            // for a fully-named series, and it is NOT optional here. Saying so in the tool's own
            // text is what stops a model treating its own refusal as a bug in the schema.
            "description": "DELETE stored data series from the datahub's history store, IRREVERSIBLY. Selects on the four series dimensions: `kind` and `venue` are required, and an OMITTED `symbol`/`group`/`interval` is a wildcard over that dimension (there are no globs — omission is the only wildcard). ⚠ `produced_by` is REQUIRED on EVERY call here, unlike the `vike-cli data history rm` command, where it is optional for a fully-named series: every commit key of every matched series must carry that prefix, and ONE foreign key refuses the whole run and deletes nothing. That is deliberate — you compose an identity from context that may be stale, and a provenance assertion is the one check that fails on a plausible-but-wrong one. WITHOUT confirm:true AND a matching preview_token this only PLANS: it returns which series matched, what each holds, and the commit keys that wrote them, with `matched`/`rows`/`bytes` as numbers to compare against what you expected. Read `list_series` first if you are unsure what the store holds. Deletion cannot be undone, and a window a venue no longer serves cannot be re-fetched at all. Requires this box's device key on the datahub's access list (role control or higher) — a key-less datahub serves no delete verb.",
            // `idempotentHint: true` is HONEST rather than reassuring: the underlying delete is
            // idempotent, so a retry after a dropped reply does not double-delete. `openWorldHint`
            // is TRUE because this reaches a datahub over a socket — which is also what withholds
            // it from the `offline` ring.
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": true }
        })),
    ]
}
