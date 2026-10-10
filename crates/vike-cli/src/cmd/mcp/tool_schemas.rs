//! The `tools/list` roster — `tools_spec`, every tool this server implements with its schema and
//! risk annotations — and the three schema-property helpers it splices into each write tool.
//!
//! Split out of `cmd/mcp.rs` (code-layout phase 2, task 9) so the longest single table in the
//! module is not interleaved with the routing code; nothing about the roster changed. The router
//! (`Server::call_tool`), the scoping rings (`ToolAccess`) and the registry-manifest gate all still
//! read this ONE function, and `scripts/gen_skills.sh` renders the `skills/` package by parsing it —
//! which is why the helpers stay column-0 `fn NAME() -> Value {` items in THIS file, beside
//! `tools_spec`, where that script's `tools.awk` looks for them.

use serde_json::{Value, json};

/// The two-call gate, defined ONCE and spliced into every write tool — so the wording cannot drift
/// between them, which is how the old inline copies ended up in four different spellings.
fn confirm_property() -> Value {
    json!({
        "type": "boolean",
        "description": "must be true AND accompanied by a valid `preview_token` to execute; either one missing returns a preview instead"
    })
}

/// The token half of the gate. See [`super::PendingPreviews`] for why it is not a secret.
fn preview_token_property() -> Value {
    json!({
        "type": "string",
        "description": "the `preview_token` returned by this tool's preview call. Fires ONCE, expires after 60s, and is BOUND to the exact command it previewed — confirming a different command with it is refused."
    })
}

/// The optional `reason` property every WRITE tool advertises — one definition, spliced into each
/// tool's `inputSchema` so the wording (and the fact that it is never required) can never drift
/// between tools.
fn reason_property() -> Value {
    json!({
        "type": "string",
        "description": "optional rationale — WHY this command is being issued. Recorded in the node's audit trail (control characters stripped, capped at 512 chars); never reaches the order, the core, or the venue."
    })
}

/// Every tool this server IMPLEMENTS — each one's schema + risk annotations.
///
/// ⚠ This is the FULL roster, not the answer to `tools/list`: a session serves
/// [`super::ToolAccess::advertised`], which is this filtered by the profile. It stays whole
/// deliberately — `server.json`'s registry listing and the `skills/` package describe the SERVER,
/// not one launch of it, and both gates read this function.
pub(super) fn tools_spec() -> Value {
    // ⚠ RENDERED from the protocol's own roster, never typed out. §15.1 of
    // `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` makes this the AGENT leg
    // of the one-roster gate: a model can only ask for a method the SCHEMA names, so a hand-typed
    // list here would leave every agent a grid user the day a fifth method shipped — which is
    // exactly the state §12 says this stage exists to end.
    let search_methods = json!(vike_datahub_client::SEARCH_METHODS);
    json!([
        {
            "name": "validate_strategy",
            "description": "Compile a Rhai strategy; returns {ok, error?}. Compile IS validation. Offline — no server.",
            "inputSchema": { "type": "object", "properties": { "script": { "type": "string", "description": "Rhai strategy source" } }, "required": ["script"] },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        },
        {
            "name": "discover_params",
            // ⚠ It reads as an INSPECTION tool for somebody else's script, and that is how the
            // first agent-eval model run read it: having authored the strategy itself, the agent
            // answered "which parameters can I tune" from its own memory of what it had just
            // written and never called this. It happened to be right and had no way to know it —
            // `validate_strategy` answers `{ok: true}` and says nothing about params, and this tool
            // runs the TOP LEVEL only, so the one mistake worth catching (a `param()` inside a
            // hook, which nothing reports) is invisible to the author too. So the text now says what
            // the tool is FOR and what it cannot see. `crates/vike-agent-eval/src/cases.rs`'s
            // `WRITE_A_RHAI_STRATEGY` is the measurement.
            //
            // ⚠ That rewrite OVERSHOT on one clause: it said a `param()` inside `on_bar()` was
            // "undrivable by a sweep", which made the tool read as a correctness gate over what a
            // grid can reach. It is false, and both halves were read back off the code before this
            // was reworded. `crates/vike-script/src/engine/host.rs`'s `param` registration reads
            // `overrides` on EVERY call rather than only during the one-time top-level run, and
            // `crates/vike-backtest/src/harness/registry.rs`'s `rhai_overrides` forwards every
            // numeric param key except `src` with no comparison against the declared set — so a
            // grid key naming a hook-buried knob reaches it and drives it. The real limitation is
            // narrower and duller: discovery never REPORTS such a knob, so nothing tells the author
            // (or whoever writes the grid) that it is there or what its default is. Claiming more
            // than that teaches an agent to conclude a working grid is inert.
            "description": "Read back the tunable param(name, default) knobs a Rhai strategy declares — the exact keys a profile's [strategy.params] or a [sweep] grid may override, in declaration order. Call it on a script you WROTE as well as one you were handed: it runs the script's TOP LEVEL only, so a param() inside on_bar() is NOT REPORTED here (a sweep can still drive such a knob by name — but nothing tells you it is there), and compiling clean says nothing about it. Offline — no server.",
            "inputSchema": { "type": "object", "properties": { "script": { "type": "string" } }, "required": ["script"] },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        },
        {
            "name": "list_templates",
            "description": "List starter Rhai strategies as {name, code} — each parameterized via param() so it drops straight into run_sweep. Offline — no server.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        },
        {
            "name": "list_indicators",
            // ⚠ This text ships in EVERY session's tools/list, called or not, so it names none of
            // the roster — see `list_indicators_description_does_not_enumerate_the_set`. It used
            // to splice `RHAI_INDICATORS.join(", ")` in, which was 13 characters when the host
            // bound three names and is kilobytes now that it binds the catalog.
            "description": "List the HOST-BOUND indicators a Rhai strategy can actually call — what the vike-script host registers, which is not every vike-indicators registry name; a script calling an unbound one compiles and then fails on every bar. No arguments returns the compact roster (name + category). Pass `category` for one family in full, or `name` for one indicator in full — its parameters with defaults and its output line — which also answers WHY a registry name is not callable. The roster is not repeated in this text on purpose: it is long, and a description is sent whether you call the tool or not.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "one indicator, in full (its parameters with defaults, and the value it returns) — or, for a registry name the host holds back, the reason" },
                    "category": { "type": "string", "description": "one family, in full — the `category` value the roster reports (case-insensitive)" }
                }
            },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        },
        {
            "name": "run_backtest",
            "description": "Run a backtest on the remote backtest daemon (`vike-backend backtest --addr`, dialled at --backtest-addr) from a profile TOML, optionally injecting a Rhai script. Returns the BacktestReport JSON.",
            "inputSchema": {
                "type": "object",
                "properties": { "profile": { "type": "string" }, "script": { "type": "string" } },
                "required": ["profile"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "run_sweep",
            "description": "Run a parameter search on the remote backtest daemon (--backtest-addr) from a profile TOML with a [paramscan] table (each key overrides strategy.params.<key> across a value grid; the legacy [sweep] spelling still loads), optionally injecting a Rhai script. DEFAULT is the exhaustive grid; `optimizer` selects a smarter search (euler = successive halving, tpe = Bayesian, genetic) and each method takes its own knob — a knob under a method that does not own it is REFUSED by name, never discarded. Returns the server-ranked ParamscanReport JSON (one row per grid point, each with its BacktestReport, best first).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "profile": { "type": "string", "description": "a backtest profile TOML with [data]/[strategy]/[paramscan] (+ optional [engine])" },
                    "script": { "type": "string", "description": "optional Rhai source injected as [strategy.params].src" },
                    "rank_by": { "type": "string", "description": "sharpe (default) | return | max_dd | equity | multi (the composite objective) — applied server-side" },
                    "optimizer": { "type": "string", "enum": search_methods, "description": "the search METHOD (CLI: --optimizer). grid (default) is exhaustive" },
                    "trials": { "type": "integer", "description": "tpe's trial budget (CLI: --trials). tpe ONLY — refused under any other optimizer" },
                    "seed": { "type": "integer", "description": "reproducibility seed (CLI: --seed). tpe and genetic ONLY; genetic REQUIRES it" },
                    "euler_depth": { "type": "integer", "description": "euler's successive-halving depth (CLI: --euler-depth). euler ONLY" }
                },
                "required": ["profile"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "run_walk_forward",
            "description": "Run an out-of-sample walk-forward on the remote backtest daemon (--backtest-addr) from a profile TOML with a [walkforward] table, optionally injecting a Rhai script. DEFAULT (n_splits alone) is the FIXED-parameter stability walk: every window trades the profile's own [strategy.params] and chooses nothing — do NOT report such a run as an optimization. search = \"sweep\" in [walkforward] runs the other protocol: each window re-scores the [sweep] grid on its OWN training half and trades only that window's winner (mode = anchored|rolling, rank_by = sharpe|return|max_dd|equity); it needs a [sweep] table and is refused without one. Returns the stitched WalkForwardReport JSON (OOS windows + oos_sharpe + wf_consistency) — only a window that searched carries chosen_params, which is how you tell the two runs apart. Run both on one profile: the no-search control is the only evidence that searching bought anything.",
            "inputSchema": {
                "type": "object",
                "properties": { "profile": { "type": "string", "description": "a backtest profile TOML with [data]/[strategy]/[walkforward] (+ optional [engine]; a [sweep] table too when [walkforward].search = \"sweep\")" }, "script": { "type": "string", "description": "optional Rhai source injected as [strategy.params].src" } },
                "required": ["profile"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "list_strategies",
            "description": "List the compiled native backtest strategies the remote backtest daemon offers (the names a profile's strategy.name can resolve). Requires a reachable backtest daemon (--backtest-addr).",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
        },
        {
            "name": "list_series",
            "description": "List every stored data series the remote vike-datahub server holds, with coverage: {kind, venue, symbol, interval, first_ts, last_ts, rows} — what a profile's [data] table can name. Requires a reachable datahub (--addr).",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
        },
        {
            "name": "node_snapshot",
            "description": "Read the running vike-tradehub node's live state (orders, positions, per-venue equity, recent events). Requires --node + VIKE_TRADEHUB_OBSERVE_KEY. If the result carries `pre_fold: true`, the frame holds nothing built: with `identity` present it is the placeholder the node publishes before its first fold; with `identity: null` it is this client's own placeholder, because no node frame arrived in time. Either way venues, balance, equity_total and accounts_epoch are placeholders, `venues: []` does NOT mean nothing is mounted, and `pre_fold_note` says which — call it again. If the node connection has dropped — a drop the socket REPORTS: the tunnel process exiting, the daemon restarting — this returns an ERROR naming it DOWN and the last frame STALE, never that frame as if live, and reconnects on the next call, so call it again rather than restarting anything. ⚠ A link that died SILENTLY (a sleeping laptop, an ssh tunnel without ServerAliveInterval) is NOT detected: the last frame is answered as live until the socket reports the drop, and the frame carries no timestamp to age it by. If the picture never changes while the node should be trading, have the operator check the tunnel before trusting it.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "strategy_status",
            // ⚠ The last clause is a DISCLOSURE, not padding. `WireMountRow` carries
            // strategy/params/live and NOTHING ELSE — no mount id, no venue/symbol/interval — and
            // an agent that assumed otherwise would plan an `unmount_strategy` off a field that is
            // not in the payload, then invent one. Saying where the id really comes from is the
            // whole difference between a read it can act on and a read it will guess past.
            "description": "Ask the running vike-tradehub node WHAT IT IS RUNNING: which daemon answered (name, live/paper, build), the daemon's resolved effective-params line, and one row per mounted strategy (strategy name, that mount's params, and whether THAT MOUNT trades live — a per-venue fact, which may disagree with the daemon's own live flag). Read-only, over the OBSERVE scope: it can neither place nor change anything. Requires --node + VIKE_TRADEHUB_OBSERVE_KEY and a node that advertises the strategy verbs; an older node is refused CLIENT-SIDE with nothing sent. ⚠ It does NOT report mount IDs — see unmount_strategy for where one comes from.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
        },
        {
            "name": "settings_show",
            // ⚠ NO subject word from `the_mcp_surface_advertises_no_credential_writer`'s SUBJECT
            // list may appear here, and the temptation is real: the natural sentence for the
            // redaction clause reaches for the word this surface may not pair with an act word.
            // "sensitive-looking" carries the same fact and trips nothing — and the redaction is
            // the NODE's anyway (`WireSettingsRow`'s doc: rows arrive already redacted, on
            // construction, so no serializer on this side could leak one).
            "description": "Read the running vike-tradehub node's EFFECTIVE settings: the settings directory it resolved at boot, then one row per typed key — which file it belongs to, its full dotted key, the effective value, the LAYER that set it (a default, a file, an environment variable), and what actually READS it. Sensitive-looking values arrive already redacted by the node. The FILES half only: the box's environment registry stays a local disclosure. Read-only, over the OBSERVE scope. Requires --node + VIKE_TRADEHUB_OBSERVE_KEY and a node that advertises the settings-show capability; an older node is refused CLIENT-SIDE with nothing sent. `rows[].key` is the exact spelling set_setting takes.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
        },
        {
            "name": "submit_order",
            "description": "Submit an order on the vike-tradehub node. WITHOUT confirm:true this only PREVIEWS (executes nothing). Requires --node + VIKE_TRADEHUB_CONTROL_KEY.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "venue": { "type": "string" }, "symbol": { "type": "string" },
                    "side": { "type": "integer", "description": "1 buy / -1 sell" },
                    "qty": { "type": "number" },
                    "order_type": { "type": "string", "description": "market | limit | stop | take_profit (default market)" },
                    "price": { "type": "number" }, "trigger_price": { "type": "number" },
                    "reduce_only": { "type": "boolean" },
                    "reason": reason_property(),
                    "confirm": confirm_property(), "preview_token": preview_token_property()
                },
                "required": ["venue", "symbol", "side", "qty"]
            },
            "annotations": { "destructiveHint": true, "idempotentHint": false, "openWorldHint": true }
        },
        {
            "name": "cancel_order",
            "description": "Cancel one resting order by client_order_id. WITHOUT confirm:true this only PREVIEWS.",
            "inputSchema": {
                "type": "object",
                "properties": { "client_order_id": { "type": "string" }, "reason": reason_property(), "confirm": confirm_property(), "preview_token": preview_token_property() },
                "required": ["client_order_id"]
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "modify",
            "description": "Modify one resting order's qty and/or price by client_order_id (at least one of new_qty/new_price). PREVIEW IS MANDATORY: WITHOUT confirm:true this only PREVIEWS (executes nothing).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "client_order_id": { "type": "string" },
                    "new_qty": { "type": "number" }, "new_price": { "type": "number" },
                    "reason": reason_property(),
                    "confirm": confirm_property(), "preview_token": preview_token_property()
                },
                "required": ["client_order_id"]
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "flatten",
            "description": "Close the (venue, symbol) net position with a reduce-only market order. WITHOUT confirm:true this only PREVIEWS.",
            "inputSchema": {
                "type": "object",
                "properties": { "venue": { "type": "string" }, "symbol": { "type": "string" }, "reason": reason_property(), "confirm": confirm_property(), "preview_token": preview_token_property() },
                "required": ["venue", "symbol"]
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "market_exit",
            "description": "PANIC BUTTON — cancel every live order then flatten every position (optionally scoped to one venue). WITHOUT confirm:true this only PREVIEWS.",
            "inputSchema": {
                "type": "object",
                "properties": { "venue": { "type": "string", "description": "omit for every engine" }, "reason": reason_property(), "confirm": confirm_property(), "preview_token": preview_token_property() }
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "mass_cancel",
            "description": "Cancel EVERY live order, optionally scoped to one venue and/or symbol. PREVIEW IS MANDATORY: WITHOUT confirm:true this only PREVIEWS (executes nothing).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "venue": { "type": "string", "description": "omit for every venue" },
                    "symbol": { "type": "string", "description": "omit for every symbol" },
                    "reason": reason_property(),
                    "confirm": confirm_property(), "preview_token": preview_token_property()
                }
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "set_trading_state",
            // ⚠ The second sentence is here because an agent REACHED FOR THIS TOOL to "switch
            // binance to live". The first agent-eval model run refused correctly — no write, no key
            // echoed — and then told the operator the remedy was "the node's config/env and a
            // restart", which cannot work: no environment variable arms a venue, and the operator
            // who follows that edits something nothing reads and stays on paper with no error. This
            // is the only tool on the roster whose name reads like "go live", and it now says what
            // it is not and where the decision actually lives.
            //
            // ⚠ It names the POLICY half and stops there, deliberately. The credential half is
            // fenced by `docs/decisions/0036…` and by
            // `the_mcp_surface_advertises_no_credential_writer` below: this description may not
            // pair a credential word with a write word, and it opens with "Set". The human-facing
            // verb lives in the `arm_a_venue` prompt and the `arm-a-venue` skill, which the scan
            // deliberately does not walk and which execute nothing.
            "description": "Set the account trading state / kill switch on the node — active | reducing | halted. WITHOUT confirm:true this only PREVIEWS. ⚠ It is NOT a venue arming control and cannot move a venue off the paper simulator: which venues may go live is the `policy.venues.<venue>` row, set with `vike-cli config set` by a human on the box, and nothing on this server can change it.",
            "inputSchema": {
                "type": "object",
                "properties": { "state": { "type": "string", "description": "active | reducing | halted" }, "reason": reason_property(), "confirm": confirm_property(), "preview_token": preview_token_property() },
                "required": ["state"]
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "mount_strategy",
            // ⚠ The `rhai` clause says "PATH ON THE NODE" twice because the obvious agent mistake
            // is to paste the script it just wrote with `validate_strategy` into this argument.
            // That string is then a filename the daemon cannot open, and the refusal arrives from
            // the node rather than from the schema — a whole round trip to learn that this surface
            // has no way to put a file on that box at all.
            "description": "Add ONE strategy mount to the running vike-tradehub node's core, LIVE and without a restart. WITHOUT confirm:true this only PREVIEWS (executes nothing). The source is the profile [strategy] vocabulary: EXACTLY ONE of `name` (a registry strategy the node compiles in) or `rhai` (a script PATH ON THE NODE's filesystem — not script source; this server cannot put a file on that box). `venue` must name an engine the node already runs — read it from node_snapshot's venues[].venue. The node validates at its edge with the same refusals a profile load applies; a refusal the core itself raises later (a duplicate mount id, an unknown venue) surfaces in the node's recent events rather than here. Requires a node that advertises the mount verbs; an older one is refused CLIENT-SIDE with nothing sent.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "venue": { "type": "string", "description": "an engine the node ALREADY runs — node_snapshot's venues[].venue, exactly (the comparison is case-sensitive)" },
                    "account": { "type": "string", "description": "WHICH ACCOUNT of `venue` this mount trades and reads — node_snapshot's venues[].account, exactly. OMIT it only where the node runs ONE account of that venue: where it runs two or more, an account-less mount is REFUSED rather than sent to the default one, because the default account's route key IS the bare venue and the silent answer would look correct. Pass \"DEFAULT\" to name the venue's unlabelled account deliberately — that is a DIFFERENT thing from omitting the field, and it is accepted at any account count" },
                    "symbol": { "type": "string", "description": "the mount's own symbol" },
                    "interval": { "type": "string", "description": "the mount's bar-series interval, e.g. `1m`" },
                    "controller_id": { "type": "string", "description": "optional explicit mount id; omitting it derives `{venue}__{symbol}__{interval}`. Whatever this resolves to is what unmount_strategy will need" },
                    "name": { "type": "string", "description": "a REGISTRY strategy name — exactly one of `name` / `rhai`" },
                    "rhai": { "type": "string", "description": "a Rhai script PATH on the NODE's filesystem — exactly one of `name` / `rhai`. NOT script source" },
                    "params": { "type": "object", "description": "the [strategy.params] table; omitted means an empty table. Each strategy reads its own knobs, so this wire never re-declares them" },
                    "reason": reason_property(),
                    "confirm": confirm_property(), "preview_token": preview_token_property()
                },
                "required": ["venue", "symbol", "interval"]
            },
            "annotations": { "destructiveHint": true, "idempotentHint": false, "openWorldHint": true }
        },
        {
            "name": "unmount_strategy",
            // ⚠ Two clauses an agent cannot get from anywhere else, and both were read off the
            // wire variant's own doc. The positions one is the safety half: "unmount" reads as
            // "stand this down", and what it really leaves behind is an OPEN POSITION with nothing
            // managing it. The mount-id one is the usability half — `strategy_status` reports no
            // id, so an agent told to "read it from the status" would go looking for a field that
            // is not in the payload.
            "description": "Remove ONE strategy mount from the running vike-tradehub node's core by MOUNT ID. WITHOUT confirm:true this only PREVIEWS. The node CANCELS that mount's attributed live orders before removing it and saves its durable state — but POSITIONS ARE NOT FLATTENED: whatever the strategy left open stays open with nothing managing it, and `flatten` is the verb for that. ⚠ The id is the explicit `controller_id` the mount was created with, or the derived `{venue}__{symbol}__{interval}` — strategy_status does NOT report mount ids, so an id you did not mint yourself has to come from the operator or from that derivation. An unknown id surfaces in the node's recent events rather than as an error here. Requires a node that advertises the mount verbs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "controller_id": { "type": "string", "description": "the mount id to remove — the explicit one given at mount time, or `{venue}__{symbol}__{interval}`" },
                    "reason": reason_property(),
                    "confirm": confirm_property(), "preview_token": preview_token_property()
                },
                "required": ["controller_id"]
            },
            "annotations": { "destructiveHint": true, "idempotentHint": false, "openWorldHint": true }
        },
        {
            "name": "set_setting",
            // ⚠ Like `set_trading_state` above, this description may name no credential: the
            // SUBJECT×ACT scan in `the_mcp_surface_advertises_no_credential_writer` would refuse
            // the whole surface for pairing one with the word `set` in this tool's own NAME. That
            // is not a wording problem to route around — the fact an agent needs (that nothing
            // here reaches a credential) is stated once, for the whole server, by
            // [`INSTRUCTIONS_NO_CREDENTIAL`], which is the right altitude for it anyway.
            //
            // ⚠ A MEASURED CONSEQUENCE of the argument being called `key`: the agent transcript
            // records its value as redacted. `crate::cmd::mcp::trace`'s key rule is
            // `vike_config::is_secret_key`, the workspace's ONE authority on a credential-shaped
            // NAME, and that function matches the bare word `KEY` — so `key`, `setting_key` and
            // every other spelling of this concept is caught. It is not fixable from here and must
            // not be: a second redaction table is precisely what that module refuses to spell.
            // What survives in the record is the tool, the `value` and the verdict; what is lost
            // is WHICH key a REFUSED attempt named (the tool's `file` argument, which used to carry
            // the section, went with the file era). The accepted case is unaffected — the node's
            // own audit trail records the write with its old and new values, which is where an
            // executed change is recorded anyway.
            // `the_transcript_redacts_the_settings_key_and_keeps_the_rest` pins both halves, so
            // this stays a known bound rather than a surprise.
            "description": "Write ONE setting on the running node: one row of its settings database, named by its full dotted `key` (policy.* | config.* | preferences.* | flags.*) and validated by the NODE's own loader before it commits — so a write can never leave a store the next boot refuses. WITHOUT confirm:true AND this call's preview_token it only PREVIEWS, and the preview's `change` shows the key's current value on the node beside the new one (old → new). ⚠ This changes a LIVE setting on the node, and a policy.* key is a risk ceiling or an arming switch: confirm one only after the owner said yes in chat. A session started with --unattended refuses every policy.* key; limits are then changed by the operator, with `vike-cli config set` or the GUI. The answer's `restart_required` says which happened: false = the node applied the value LIVE, true = it applies at the next restart (every policy.* key — policy is never hot). Read settings_show first: `rows[].key` is the exact spelling this takes, and `rows[].origin` says whether an environment variable outranks the stored value. Requires a node that advertises the settings-write capability; an older one is refused CLIENT-SIDE with nothing sent.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "the FULL dotted key exactly as settings_show renders it (`config.tradehub_addr`, `policy.max_notional_per_order`); its first segment names the section" },
                    "value": { "type": "string", "description": "the new value as TEXT — parsed as a TOML value (`250`, `true`, `[\"a\"]`) when it is one, else written as a string, and then the row goes through the node's loader" },
                    "reason": reason_property(),
                    "confirm": confirm_property(), "preview_token": preview_token_property()
                },
                "required": ["key", "value"]
            },
            "annotations": { "destructiveHint": true, "idempotentHint": false, "openWorldHint": true }
        },
        {
            "name": "delete_series",
            // ⚠ The description carries the ASYMMETRY with the CLI, because nothing else can: an
            // agent reading `vike-cli data hist rm`'s help would learn that `--produced-by` is optional
            // for a fully-named series, and it is NOT optional here. Saying so in the tool's own
            // text is what stops a model treating its own refusal as a bug in the schema.
            "description": "DELETE stored data series from the datahub's history store, IRREVERSIBLY. Selects on the four series dimensions: `kind` and `venue` are required, and an OMITTED `symbol`/`group`/`interval` is a wildcard over that dimension (there are no globs — omission is the only wildcard). ⚠ `produced_by` is REQUIRED on EVERY call here, unlike the `vike-cli data hist rm` command, where it is optional for a fully-named series: every commit key of every matched series must carry that prefix, and ONE foreign key refuses the whole run and deletes nothing. That is deliberate — you compose an identity from context that may be stale, and a provenance assertion is the one check that fails on a plausible-but-wrong one. WITHOUT confirm:true AND a matching preview_token this only PLANS: it returns which series matched, what each holds, and the commit keys that wrote them, with `matched`/`rows`/`bytes` as numbers to compare against what you expected. Read `list_series` first if you are unsure what the store holds. Deletion cannot be undone, and a window a venue no longer serves cannot be re-fetched at all. Requires a datahub that holds node keys — a key-less one serves no delete verb.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "description": "the EXACT stored kind (bar | quote | trade | book | depth | properties | …), never a substring" },
                    "venue": { "type": "string", "description": "the EXACT venue slug, never a substring. ⚠ It names an EXCHANGE, never a data SOURCE — rows fetched from a metrics vendor land under the exchange they describe, which is why `produced_by` is the only thing that tells two producers of one series apart" },
                    "symbol": { "type": "string", "description": "the EXACT symbol of a PER-SYMBOL series. Omit to wildcard the dimension. Alternative to `group`, never a pair" },
                    "group": { "type": "string", "description": "the EXACT group of a GROUPED series (which holds many symbols in one part and has NO symbol of its own). Alternative to `symbol`" },
                    "interval": { "type": "string", "description": "the EXACT bar interval (`1h`). Omit to wildcard; refused together with `group`, whose leaf has no interval segment" },
                    "produced_by": { "type": "string", "description": "REQUIRED. The commit-key PREFIX every key of every matched series must carry (`panel_bars:`, `pmxt:quote:`). ⚠ A LITERAL prefix, never a producer PATH: this tool deletes through a datahub, and a path is refused here before anything is dialled. Read the prefix off `list_series` — that is evidence from the store, where a path is a guess about a source tree you cannot see; and a datahub that has not been redeployed since 2026-09-11 resolves no path at all, so one sent there would be asserted literally, match no key, and report the store as foreign when in fact the argument was never resolved. One key that does not carry it refuses the whole run. There is no override and no --force." },
                    "reason": reason_property(),
                    "confirm": confirm_property(), "preview_token": preview_token_property()
                },
                "required": ["kind", "venue", "produced_by"]
            },
            // `idempotentHint: true` is HONEST rather than reassuring: the underlying delete is
            // idempotent, so a retry after a dropped reply does not double-delete. `openWorldHint`
            // is TRUE because this reaches a datahub over a socket — which is also what withholds
            // it from the `offline` ring.
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": true }
        }
    ])
}
