//! The MCP prompts: three instruction sheets, each scoped by the session's tool access.

use serde_json::{Value, json};

#[cfg(doc)]
use super::Server;
use super::scope::ToolAccess;
use super::{PREVIEW_WINDOW, WRITE_TOOLS};

// ---- prompts ---------------------------------------------------------------------------------

/// The two-call gate, spelled ONCE for the prompts that describe it.
///
/// ⚠ Written from [`Server::call_tool`]'s own behaviour, deliberately not from any published page:
/// the pages describing this surface said `confirm: true` was the whole gate, which stopped being
/// true when the token binding landed — so a prompt copied from one would teach an agent to be
/// refused on its second call and to have no idea why. Every clause below is a branch you can read
/// in that function.
///
/// ⚠ A FUNCTION rather than a `const`, and that is the whole point: the roster comes from
/// [`WRITE_TOOLS`] and the window from [`PREVIEW_WINDOW`], because this is the ONE document in the
/// tree that teaches an agent WHICH tools need two calls. Written out by hand it was a fourth
/// spelling of the roster held equal to nothing — so an eighth write tool would join the routing,
/// the `destructiveHint` annotations and the transcript harness automatically and be missing from
/// exactly the sheet an agent reads before calling it once and believing it executed.
/// `every_write_touching_prompt_teaches_the_token_not_just_confirm` walks the roster over the
/// rendered text, so a re-hardcoding is caught as well as prevented.
pub(super) fn two_call_gate(access: &ToolAccess) -> String {
    // ⚠ The roster is the ADMITTED write set, not [`WRITE_TOOLS`] whole. Under a scoped profile the
    // sheet must describe the session the agent is actually in: teaching the two-call gate for tools
    // this server does not serve is teaching a procedure whose first call is a refusal. When NONE is
    // served there is no gate to teach at all, and [`scope_banner`] — which every prompt carries at
    // the TOP, where it is read before the steps rather than after them — says so instead.
    let served: Vec<&str> = WRITE_TOOLS.iter().copied().filter(|t| access.admits(t)).collect();
    if served.is_empty() {
        return String::new();
    }
    format!(
        "\
Every order-write tool ({roster}) takes TWO calls, and the first one sends nothing:

1. Call the tool with its arguments and NO `confirm`. You get back `will_execute: false`, the \
resolved `wire_command`, a `guardrail` estimate, a `node_verdict`, and a `preview_token`.
2. READ THE VERDICT BEFORE CONFIRMING. `verified_by_node: true` means the node itself dry-ran the \
command against the real ControlLimits and RiskGate — that is the verdict that counts. \
`verified_by_node: false` means NO verdict came back: either the node was never asked (none \
configured, no control key) or it was asked and did not answer (unreachable, handshake refused, \
or it returned an error). `node_verdict.reason` says which. In that case `guardrail` is an \
unverified client-side estimate; it cannot price a MARKET order at all, because a market order \
carries no price, and market is the default order type. Do not read an absent verdict as approval.
3. Call the SAME tool again with the same arguments plus BOTH `confirm: true` AND the exact \
`preview_token` from step 1.

`confirm: true` on its own is not a confirmation — it returns another preview. A token fires at \
most ONCE, expires {secs} seconds after it was issued, and is BOUND to the command it previewed: \
confirming different arguments with it is refused, so preview the command you actually intend to \
send. (The one field excluded from that binding is `client_order_id`, because this server mints a \
fresh one per call; the order that reaches the venue carries the id the preview displayed.)

The optional `reason` argument on every write tool is recorded in the node's audit trail and never \
reaches the order, the core or the venue. Fill it in.",
        roster = served.join(", "),
        secs = PREVIEW_WINDOW.as_secs(),
    )
}

/// The line every prompt opens with when this session serves NO order-write tool — `None` under a
/// profile that serves at least one.
///
/// ⚠ It sits at the TOP of the sheet, before the steps, and that placement is the point. The steps
/// of `triage_a_stuck_order` legitimately name `modify` and `cancel_order` — they are the right
/// answer to a stuck order — so a sheet that only mentioned the scope at the END would have an
/// agent plan two calls it cannot make before reading that it cannot make them. It also states that
/// the agent cannot widen this itself, because the failure mode of a refusal without that clause is
/// an agent that spends its next turns hunting for the switch.
fn scope_banner(access: &ToolAccess) -> Option<String> {
    if WRITE_TOOLS.iter().any(|t| access.admits(t)) {
        return None;
    }
    Some(format!(
        "⚠ THIS SESSION SERVES NO ORDER-WRITE TOOL. The server is running under the `{profile}` \
         tool profile, so every write tool named below is absent from `tools/list` and is refused \
         if called: nothing you do here can place, modify or cancel an order. Only the OPERATOR can \
         change that, by restarting the server with `--profile full` in the MCP client's launch \
         command — so where a step below says to act, READ and REPORT instead, and say plainly what \
         a human would have to run.",
        profile = access.profile_name(),
    ))
}

/// The `prompts/list` payload — `(name, description, arguments)` per flow.
///
/// Three flows, because these are the three an agent is asked for and gets wrong differently: a
/// backtest is a loop it can run alone, arming a venue is a decision it must NOT take alone, and a
/// stuck order is the case where reading before acting matters most.
pub(super) fn prompts_spec() -> Value {
    json!([
        {
            "name": "backtest_a_strategy",
            "description": PROMPT_BACKTEST_DESC,
            "arguments": [
                { "name": "idea", "description": "the strategy idea in one sentence, if you have one", "required": false }
            ]
        },
        {
            "name": "arm_a_venue",
            "description": PROMPT_ARM_DESC,
            "arguments": [
                { "name": "venue", "description": "the venue id, e.g. binance / bybit / okx / hyperliquid", "required": false }
            ]
        },
        {
            "name": "triage_a_stuck_order",
            "description": PROMPT_TRIAGE_DESC,
            "arguments": [
                { "name": "client_order_id", "description": "the coid of the order in question, if known", "required": false }
            ]
        }
    ])
}

const PROMPT_BACKTEST_DESC: &str = "Author, validate and backtest a Rhai strategy end to end, using only the offline tools and \
     the remote datahub — no node, no orders.";
const PROMPT_ARM_DESC: &str = "Understand what actually arms a venue for live trading, and what this agent surface can and \
     cannot do about it.";
const PROMPT_TRIAGE_DESC: &str = "Diagnose an order that is not behaving — read the node's state first, then act through the \
     two-call preview gate.";

/// The `description` a `prompts/get` echoes back, held equal to the roster's by construction.
pub(super) fn prompt_description(name: &str) -> Value {
    match name {
        "backtest_a_strategy" => json!(PROMPT_BACKTEST_DESC),
        "arm_a_venue" => json!(PROMPT_ARM_DESC),
        "triage_a_stuck_order" => json!(PROMPT_TRIAGE_DESC),
        _ => Value::Null,
    }
}

/// Render one prompt's instruction text. PURE — a prompt describes a flow, it does not run one.
///
/// Arguments are all OPTIONAL: an agent that calls `prompts/get` with a name alone gets a usable
/// sheet with the specifics left as placeholders, which is the shape a human browsing a client's
/// prompt menu actually gets.
pub(crate) fn render_prompt(
    name: &str,
    args: &Value,
    access: &ToolAccess,
) -> Result<String, String> {
    let arg =
        |key: &str| args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    // Rendered once for the arms that splice it — the roster, the window and now the SCOPE it names
    // are all DERIVED (see [`two_call_gate`]), which is why it is a value here rather than a `const`
    // in scope.
    let gate = two_call_gate(access);
    let body = match name {
        "backtest_a_strategy" => {
            let idea = arg("idea").unwrap_or("the strategy idea you were given");
            Ok(format!(
                "Backtest {idea}, in this order. Every tool here is offline or read-only; none of \
                 it can place an order.

1. `list_indicators` with no arguments for the compact roster of what a Rhai strategy can \
                 actually CALL, then again with `name` or `category` for the ones you want in \
                 full. ⚠ This is NOT the whole indicator registry: a script calling a name the host \
                 does not bind compiles fine and then fails on EVERY bar, so the strategy looks \
                 mounted and never trades. If a name you want is missing, ask `list_indicators` \
                 for it by `name` — the answer says why it is held back.
2. `list_templates` for starter strategies. Each is parameterised with `param(name, default)` so \
                 it drops straight into a sweep.
3. Write the strategy, then `validate_strategy`. A compile error comes back as `ok: false` with \
                 the message, not as a tool error — read it and fix the script.
4. `discover_params` to see the knobs the script declares, so the profile's `[strategy.params]` \
                 and any `[sweep]` grid name keys that exist.
5. `list_series` for what data the datahub actually holds — `{{kind, venue, symbol, interval, \
                 first_ts, last_ts, rows}}` — and pick a `[data]` range inside the coverage it \
                 reports. `list_strategies` if you would rather use a compiled native strategy \
                 than a script.
6. `run_backtest` with the profile TOML and the script. The report is also readable afterwards as \
                 the `vike://backtest/last` resource, for the rest of this session only.
7. If the result looks good, do NOT stop there. `run_sweep` shows whether it survives its own \
                 parameter grid, and `run_walk_forward` shows whether it survives out of sample. A \
                 single in-sample backtest is the weakest evidence this server can produce.
8. Read `run_walk_forward`'s two forms as two different claims. With `n_splits` alone it \
                 trades YOUR parameters in every window — evidence that those settings held \
                 up, and nothing about a procedure. With `search = \"sweep\"` in \
                 `[walkforward]` each window re-fits on its own training half and trades only \
                 its winner, which is the claim people usually mean by \"walk-forward\". Run \
                 BOTH and report the pair: the no-search control is the only thing that says \
                 whether the fitting bought anything, and on our own data it has come back \
                 saying it bought nothing.

Report the numbers you got, including the bad ones."
            ))
        }
        "arm_a_venue" => {
            // A placeholder rather than a phrase, because it is spliced into COMMAND LINES below as
            // well as into prose — `--venue the venue` would read as a real argument.
            let venue = arg("venue").unwrap_or("<venue>");
            Ok(format!(
                "You have been asked about arming {venue} for live trading. Read this before \
                 doing anything.

⚠ THIS AGENT SURFACE CANNOT ARM A VENUE, and that is deliberate. Arming is a settings-database \
                 write and a credential decision a human makes; there is no tool here that does it, \
                 and there should not be.

What actually decides, in the order the mount consults it:

1. The ACCOUNT ROW: {venue}'s row in the settings database's account table, with its `tier` \
                 (`paper` | `demo` | `live`) and its `active` flag. An ACTIVE row trades at its own \
                 tier from the next restart; no row, an inactive row or tier `paper` is the paper \
                 simulator. `vike-cli secrets accounts` lists the rows with their ids, and a human \
                 changes them with `vike-cli secrets account add --venue {venue} --tier <tier> \
                 --no-label`, `account set-tier --id N --tier <tier>`, `account activate --id N` and \
                 `account deactivate --id N`. ⚠ For binance, bybit, okx and hyperliquid tier `live` \
                 means MAINNET, so an active `live` row with that venue's LIVE keys in the store is \
                 real money: a deliberate choice, never a default. Two ACTIVE rows of one venue and \
                 label at `demo` AND `live` trade neither: that account stays paper until one is \
                 deactivated.
2. The credentials of that tier. Absent credentials ARE the live gate — the venue stays on the \
                 paper simulator. ⚠ Saving a credential creates its account row ACTIVE, so storing a \
                 key set is by itself enough to arm at the next restart. `vike-cli secrets path` \
                 prints which store this project resolves to and `vike-cli secrets list` prints the \
                 key NAMES in it (never the values).

So the human's job is: leave exactly ONE active row of that venue at the tier they mean (and \
                 deactivate or re-tier the others), put that tier's key set in the store, and \
                 restart. Yours is to tell them which of the two is missing.

What you CAN do here, once someone else has armed it:
- `node_snapshot` (or the `vike://node/snapshot` resource) to read what the node is actually doing.
- `set_trading_state` to move the account between `active` / `reducing` / `halted`. ⚠ That is the \
                 KILL SWITCH, not an arming control — it is a write tool and goes through the \
                 gate below like any other.

{gate}"
            ))
        }
        "triage_a_stuck_order" => {
            let which = arg("client_order_id")
                .map(|c| format!("the order with client_order_id {c:?}"))
                .unwrap_or_else(|| "an order that is not behaving".to_string());
            Ok(format!(
                "Triage {which}. READ FIRST — every step below that changes anything is a write \
                 tool behind the two-call gate.

1. `node_snapshot` (or read the `vike://node/snapshot` resource). Find the order in the live \
                 order list and look at its state, its filled quantity and the recent events. \
                 Report what you SEE before proposing anything.
2. Decide which of these it actually is, because they need opposite responses:
   - RESTING and simply not filling — the price is away from the market. `modify` its price or \
                 `cancel_order` it. Nothing is wrong.
   - GONE from the node but believed live at the venue, or present at the venue and not on the \
                 node — that is a reconciliation divergence, not a stuck order. Do NOT paper over \
                 it by submitting a replacement: you would double the position. Report it.
   - Its outcome came back UNKNOWN (the node did not answer in time, or the control connection \
                 dropped). ⚠ The command MAY HAVE EXECUTED. Read `node_snapshot` again BEFORE \
                 retrying anything — a retry here is how one order becomes two. A dropped \
                 connection is reopened by your next call; nothing needs restarting, and \
                 `node_snapshot` errors rather than answering from a stale frame while it is down. \
                 EXPECTED after any pause longer than five minutes: the node closes an idle \
                 control connection, and the first write after the pause answers UNKNOWN even \
                 though the node had stopped reading before it was sent — read `node_snapshot`, \
                 preview again, confirm again; that is a timer, not a fault. ⚠ Only a drop the \
                 socket REPORTS is detected: a link that died silently (a sleeping laptop, an ssh \
                 tunnel without ServerAliveInterval) leaves `node_snapshot` answering its last \
                 frame as live. If the picture never changes while the node should be trading, \
                 say so and have the operator check the tunnel rather than trusting it.
3. Only then act, one command at a time, re-reading `node_snapshot` between them. Put your \
                 reasoning in each write tool's `reason` argument; it lands in the node's audit \
                 trail.

⚠ `market_exit` cancels every live order and flattens every position. It is the panic button, not \
                 a triage step. Do not reach for it because one order is confusing.

{gate}"
            ))
        }
        // Test-only, pinning the `prompts/get` guard — see `vike://__test_panic` on the resource
        // side. Never in `prompts_spec`.
        #[cfg(test)]
        "__test_panic" => panic!("kaboom"),
        other => Err(format!(
            "unknown prompt: {other:?} — call prompts/list for what this server serves"
        )),
    }?;
    // The scope banner goes FIRST, on every sheet — see [`scope_banner`] for why the position is
    // load-bearing rather than cosmetic.
    Ok(match scope_banner(access) {
        Some(banner) => format!("{banner}\n\n{body}"),
        None => body,
    })
}
