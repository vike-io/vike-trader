---
name: trade-on-a-node
description: Place, change or cancel orders on a running vike-tradehub node through the vike-cli mcp write tools (submit_order, cancel_order, modify, flatten, mass_cancel, market_exit, set_trading_state) using the mandatory two-call preview gate — preview first, then confirm with the preview_token — reading node_snapshot before and after, and settling an unknown outcome before any retry. Use when asked to buy, sell, place a market, limit, stop or take-profit order, cancel or amend a resting order, flatten or close a position, cancel all orders, halt or resume trading, hit the kill switch, or get out of everything on a live or paper vike node.
metadata:
  tools: "node_snapshot submit_order cancel_order modify flatten mass_cancel set_trading_state market_exit"
  source: "ai/trader/trading ai/trader/tools/submit_order ai/trader/tools/cancel_order ai/trader/tools/modify ai/trader/tools/flatten ai/trader/tools/mass_cancel ai/trader/tools/set_trading_state ai/trader/tools/market_exit ai/trader/tools/node_snapshot trader/guides/connect-an-agent"
---

# Trade on a node

Eight tools talk to a running `vike-tradehub` node: one read (`node_snapshot`) and seven writes.
Every write is preview-gated — a first call sends nothing, and only a second call carrying BOTH
`confirm: true` AND the `preview_token` the first call returned reaches the node
(`crates/vike-cli/src/cmd/mcp.rs`'s `Server::call_tool`). Anything less is another preview, not an
error. The node's own limits and the core risk gate are what enforce; the client-side preview is
advisory (`https://vike.io/docs/trader/guides/connect-an-agent`).

## Preconditions

- The server was started as `vike-cli mcp --node <host:port>`. Without `--node`, `node_snapshot`
  returns a clean error and every write tool still previews; what errors is a call that passes the
  gate and finds no node to send to (`https://vike.io/docs/trader/guides/connect-an-agent`).
- `VIKE_TRADEHUB_OBSERVE_KEY` for the read, `VIKE_TRADEHUB_CONTROL_KEY` for the writes — from the
  process environment, else from the node's own credential store. A missing key is a tool error
  naming both places (`https://vike.io/docs/ai/trader/tools/node_snapshot`).
- Nothing here can arm a venue: a venue with no credentials mounts as paper, and the per-venue
  policy ceiling can only ever refuse (`https://vike.io/docs/ai/trader/trading`).

## The gate — applies to every write tool

1. Call the tool with its arguments and NO `confirm`. The response has `will_execute: false`, the
   resolved `wire_command`, an advisory `guardrail`, `node_verdict`, `verified_by_node`, the echoed
   `reason`, a `note`, and the `preview_token` (`crates/vike-cli/src/cmd/mcp.rs`'s `preview_of`).
2. Read `node_verdict.checked_by`. `"node"` means the node answered and `accepted` is its own
   judgement; `"none"` means it could not be asked and `reason` names the fault; a `null`
   `node_verdict` means no `--node` or no control key. `verified_by_node` is `true` exactly when
   `checked_by` is `"node"` — a fault is never a verdict. Never read an absent verdict as approval
   (`https://vike.io/docs/ai/trader/trading`).
3. If the preview is what you intend, call the SAME tool with the SAME arguments plus BOTH
   `confirm: true` (the JSON boolean — the string `"true"`, `1` and `"yes"` fall through to a
   preview) AND `preview_token: "<token from step 1>"`.
4. Read `will_execute` / `outcome` in the reply. `confirm: true` without a token, or a token without
   `confirm: true`, returns a FRESH preview with a new token and sends nothing — it is not an error
   (`Server::call_tool`; `https://vike.io/docs/ai/trader/tools/submit_order`).

Token rules, each enforced in `Server::call_tool` and `PendingPreviews`:

- Fires once: `PendingPreviews::take` removes it before execution; a repeat gets the error
  `unknown or already used`. Read that error as possibly-already-sent — settle it with
  `node_snapshot`, do not re-confirm (`https://vike.io/docs/ai/trader/tools/submit_order`).
- Expires after 60 s (`PREVIEW_WINDOW`) — an expired token is consumed as it is rejected. Take a
  fresh preview.
- Bound to the command it previewed: `same_intent` compares the whole wire command, ignoring only
  the minted `client_order_id`. Confirming a different command is refused and consumes the token.
  Changing any argument between preview and confirm means a new preview.
- Dies with the `vike-cli mcp` process (`https://vike.io/docs/ai/trader/trading`).

What executes is the STORED previewed command, so the `client_order_id` shown in the preview is
the one sent; you do not pass it back (`Server::call_tool`).

## Procedure

1. **Read the board first** — `node_snapshot` (no arguments). Check `trading_state`, `orders`,
   `positions`, `venues[]`, and `fault`: a non-null `fault` means the core is in safe state with
   every engine halted (`https://vike.io/docs/ai/trader/tools/node_snapshot`). Do not open risk into a faulted
   node.
2. **Pick the verb** from the table below and build its arguments. Pass a `reason` — it is recorded
   in the node's audit trail (control characters stripped, capped at 512 chars) and never reaches
   the order (`tools_spec`'s `reason_property`).
3. **Preview** (gate step 1), read `wire_command`, `guardrail` and `node_verdict`.
4. **Confirm** (gate step 3) within 60 s, with both fields.
5. **Classify the outcome** using the table under Outcomes.
6. **Watch the result** with `node_snapshot` — acceptance means the node took the command, not
   that an order filled or left the book.

## Verb reference (argument names from `tools_spec`)

| Tool | Required | Optional | Does |
| --- | --- | --- | --- |
| `submit_order` | `venue`, `symbol`, `side` (`1` buy / `-1` sell), `qty` | `order_type` (`market` default, `limit`, `stop`, `take_profit`), `price`, `trigger_price`, `reduce_only`, `reason` | Submit one order — the one verb that opens risk |
| `cancel_order` | `client_order_id` | `reason` | Cancel one resting order by id |
| `modify` | `client_order_id`, and at least one of `new_qty` / `new_price` | `reason` | Change a resting order's qty and/or price |
| `flatten` | `venue`, `symbol` | `reason` | Close that net position with a reduce-only market order sized on the node |
| `mass_cancel` | none | `venue` (omit = every venue), `symbol` (omit = every symbol), `reason` | Cancel every live order in scope |
| `set_trading_state` | `state` (`active` / `reducing` / `halted`, lower case) | `reason` | The account kill switch, applied to every engine |
| `market_exit` | none | `venue` (omit = every engine), `reason` | PANIC BUTTON: cancel every live order, then flatten every one-way position |

Every write also takes `confirm` and `preview_token`. A missing required field is a tool error
before any command is built (`https://vike.io/docs/ai/trader/tools/submit_order`).

Per-verb rules that change what you send:

- `submit_order`: a market order carries no price, so the client-side `guardrail` notional is
  `null` — only `node_verdict` can size it. The node refuses an empty `client_order_id`; the server
  mints one before the preview, so omit it (`https://vike.io/docs/ai/trader/tools/submit_order`).
- `modify`: name `new_price` whenever you name `new_qty` — on a node with a notional ceiling, a
  qty change with no price is refused (`https://vike.io/docs/ai/trader/tools/modify`). A re-priced amend needs a
  re-preview.
- `flatten`: no quantity argument; it closes `|position|` resolved at apply time and is a no-op
  when flat. Partial close is a `reduce_only` `submit_order` (`https://vike.io/docs/ai/trader/tools/flatten`).
- `mass_cancel`: `symbol` without `venue` cancels NOTHING — the intent is ignored and only surfaced
  to recent events; the call still succeeds. Always name `venue` with `symbol`
  (`https://vike.io/docs/ai/trader/tools/mass_cancel`).
- `cancel_order`: accepted means the node took the command; an id that never existed is
  acknowledged the same way. Confirm removal with `node_snapshot`
  (`https://vike.io/docs/ai/trader/tools/cancel_order`).
- `set_trading_state`: `reducing` denies non-reducing orders; `halted` denies new orders but still
  admits a position-covered reduce. `active` does NOT clear the separate HALT file sentinel, and
  re-arming a latched drawdown against the same high-water mark can re-latch it
  (`https://vike.io/docs/ai/trader/tools/set_trading_state`).

## Outcomes of a confirmed call

`Server::execute` waits `ACK_WAIT` (2 s) for the node's answer to THIS command
(`crates/vike-cli/src/cmd/mcp.rs`):

| Reply | Meaning | Do |
| --- | --- | --- |
| `sent: true`, `outcome: "accepted"`, `client_order_id` | node took the command (empty id = account-wide verb) | `node_snapshot` to watch it |
| error `node rejected the command: <reason>` | the node's own refusal — usually rate or notional | read the reason; fix, re-preview |
| `sent: true`, `outcome: "unknown"` | no answer within 2 s — MAY STILL EXECUTE | `node_snapshot` BEFORE any retry |
| error saying the control connection dropped and the outcome is UNKNOWN | MAY HAVE EXECUTED | `node_snapshot` BEFORE any retry |

The two ambiguous cases arrive in different shapes — one a successful result, one an `isError`.
Inspecting only `outcome` misses the second (`https://vike.io/docs/ai/trader/trading`).

## What an UNKNOWN outcome obliges

1. Do not retry, re-confirm, or re-preview the same intent yet.
2. Call `node_snapshot` and look for the effect: the `client_order_id` in `orders`, the position in
   `positions` / `venues[].positions`, the state in `trading_state`, the tail in `recent_events`.
3. Only if the effect is absent, take a fresh preview and confirm again. Retrying blind is how one
   intent becomes two orders; a repeated `submit_order` id leaves one registry row but two orders on
   a paper mount (`https://vike.io/docs/ai/trader/tools/node_snapshot`, `https://vike.io/docs/ai/trader/trading`).
4. Remember the snapshot is lossy — the latest state, never every intermediate one; `recent_events`
   is a tail, not a ledger (`https://vike.io/docs/ai/trader/tools/node_snapshot`).

## `market_exit` is the panic button

Use it when the ask is "get out of everything" or an incident (feed outage, faulted node, runaway
strategy). Its `tools_spec` description names it the PANIC BUTTON: it cancels every live order and
then flattens every position, optionally scoped to one `venue`.

1. Preview `market_exit` with the `venue` scope (or none) and a `reason`. It is gated like every
   other write — `confirm: true` alone hands back another preview and does not fire the exit
   (`https://vike.io/docs/ai/trader/tools/market_exit`).
2. Confirm with both `confirm: true` and the `preview_token`, inside the 60 s window.
3. Read `will_execute` / `outcome`. Accepted carries an empty `client_order_id`.
4. Call `node_snapshot`. The exit is best-effort, not atomic: a resting order can still fill after
   a flatten leg, so RE-ISSUE the exit (fresh preview, fresh confirm) if the board is not flat.
5. Hedge-mode `LONG`/`SHORT` legs are skipped — on an account with only hedge buckets the
   mass-cancel runs but the flatten half mints nothing; report that rather than assuming flat.

You do not need `set_trading_state active` to escape a halt: the exit works from a halted core,
because a position-covered reduce is admitted under `Halted`. Reach for `market_exit`, not for
un-halting (`https://vike.io/docs/ai/trader/tools/set_trading_state`). One residual: a position off the lot
grid rounds its own leg up past itself and that leg is denied `halted`
(`https://vike.io/docs/ai/trader/tools/market_exit`).

## Do not

- Treat a `confirm: true` reply as evidence you traded without reading `will_execute`.
- Reuse a token, confirm a changed command with an old token, or hold a token past 60 s.
- Retry after `outcome: "unknown"`, a dropped-connection error, or `unknown or already used`
  without a `node_snapshot` first.
- Send a `modify` with `new_qty` and no `new_price`, or a `mass_cancel` with `symbol` and no
  `venue`.
- Assume the preview's `guardrail` is a pass when `node_verdict.checked_by` is `"none"`.
