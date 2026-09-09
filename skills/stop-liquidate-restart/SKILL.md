---
name: stop-liquidate-restart
description: Stop trading safely on a running vike-tradehub node from an agent — halt new risk (set_trading_state halted / reducing / active), cancel every resting order (mass_cancel), close positions (flatten, market_exit), and restart cleanly. Use when asked to halt, pause, stop, kill-switch, emergency-exit, panic, go flat, liquidate, close everything, cancel all orders, resume trading, or restart a node — and to know what a stop leaves at the venue and what a restarted node does not recover. Every write goes through the two-call preview_token gate.
metadata:
  tools: "node_snapshot set_trading_state mass_cancel flatten market_exit"
  source: "trader/guides/stop-liquidate-restart ai/trader/tools/set_trading_state ai/trader/tools/market_exit ai/trader/tools/flatten ai/trader/tools/mass_cancel"
---

# Stop, liquidate, restart

Three verbs, three different outcomes. A **stop** ends the process and, by default, leaves both
resting orders and positions at the venue. A **liquidation** is the kill switch: cancel everything,
flatten everything, halt. A **restart** recovers less than most operators assume — the venue's books
come back through reconciliation, not replay. None of the three is a side effect of another
(`https://vike.io/docs/trader/guides/stop-liquidate-restart`).

Pick the outcome first, then follow the matching procedure. All write tools below need
`vike-cli mcp --node <host:port>` plus `VIKE_TRADEHUB_CONTROL_KEY`; `node_snapshot` needs
`VIKE_TRADEHUB_OBSERVE_KEY` (`crates/vike-cli/src/cmd/mcp.rs`'s `tools_spec`).

## The two-call gate (applies to every write below)

Read from `crates/vike-cli/src/cmd/mcp.rs`'s `Server::call_tool`:

1. Call the tool WITHOUT `confirm`. Nothing is sent. The result is a preview: `will_execute: false`,
   the resolved `wire_command`, `node_verdict`, `verified_by_node`, and a `preview_token`. Read
   `node_verdict.checked_by`, not the null-ness of the verdict: `"node"` means the node ran its own
   dry-run and `accepted` is its judgement — only `accepted: true` is a verified pass; `"none"`
   means the node could not be asked (a handshake or transport fault — `reason` names it) even
   though a verdict object is present; `node_verdict: null` means no `--node` or no control key.
   `verified_by_node` is `true` exactly when `checked_by` is `"node"`. On any other path the
   guardrail is an unverified client-side estimate, not approval, and the connection is what to
   fix before confirming (`Server::node_preview`, `preview_of`).
2. Call again with the SAME arguments plus BOTH `confirm: true` AND that exact `preview_token`.
3. The token fires once, expires after 60 s (`PREVIEW_WINDOW`), and is bound to the command it
   previewed — a different command with it is refused. `confirm: true` on its own is NOT an error
   and NOT an execution: it returns ANOTHER PREVIEW. Always read `will_execute` before believing
   anything moved.
4. Outcomes (`Server::execute`): `outcome: "accepted"` with an empty `client_order_id` (an
   account-wide verb); an `isError` result on a refusal; `outcome: "unknown"` after the 2 s
   `ACK_WAIT`; or an error saying the control connection dropped. **UNKNOWN means the command MAY
   HAVE EXECUTED — call `node_snapshot` BEFORE retrying anything.**

## Procedure A — halt new risk without closing anything

Use when the user wants to stop the node opening risk but keep the book.

1. `node_snapshot` (no arguments) — record the current `trading_state`, open orders and positions.
2. `set_trading_state` with `state: "halted"` and a `reason` — preview call (no `confirm`).
3. Read `will_execute` (false), `node_verdict`, `preview_token`.
4. `set_trading_state` again with `state: "halted"`, the same `reason`, `confirm: true`,
   `preview_token: <token>`.
5. `node_snapshot` — the result shows as `trading_state` at the top level and
   `venues[].trading_state` per venue (`https://vike.io/docs/ai/trader/tools/set_trading_state`).

What the three states mean (`https://vike.io/docs/ai/trader/tools/set_trading_state`; the values are exactly
`active | reducing | halted`, lower case, per `tools_spec`):

- `active` — normal trading.
- `reducing` — only position-reducing orders are allowed; a non-reducing order is denied with the
  reason `reduce-only`.
- `halted` — no new orders (kill switch), with one deliberate exception: a position-covered reduce
  (side opposite the position, size within it) is still admitted, so a halt stops you opening risk
  without trapping you in it.

One call moves EVERY engine on the node, not one venue. `set_trading_state` is never size-capped.
A halt closes nothing and cancels nothing: resting orders keep resting under `halted`. It also does
not write the cross-process HALT file, and `active` does not clear that file — the file sentinel is
a separate kill switch outside this tool surface (`https://vike.io/docs/ai/trader/tools/set_trading_state`).

## Procedure B — clear the order book, keep the positions

1. `node_snapshot` — see what is resting.
2. `mass_cancel` — preview call. Arguments: `venue` (omit for every venue), `symbol` (omit for
   every symbol), `reason`. Omit both for the whole account.
3. `mass_cancel` again with the same scope, `confirm: true`, `preview_token: <token>`.
4. `node_snapshot` — confirm the orders are gone; the snapshot is the real answer either way.

Scope rules (`https://vike.io/docs/ai/trader/tools/mass_cancel`): venue+symbol clears that engine and that
`(venue, symbol)` conditional book; venue alone clears that engine and that venue's conditional
books; neither clears all engines and all conditional books. **A `symbol` with NO `venue` cancels
nothing** — the intent is ignored, surfaced only to the node's recent-events, and the tool call still
succeeds. Always name the `venue` when you name a `symbol`. `mass_cancel` also clears the
core-owned emulated conditionals (stops, trailing orders); it is the only way an armed emulated
conditional goes away from this surface.

## Procedure C — the kill switch (liquidate)

`market_exit` is the panic button: cancel every live order, then flatten every position, optionally
scoped to one venue (`tools_spec`). Order of actions:

1. `node_snapshot` — record positions and orders before acting.
2. `market_exit` — preview call. Arguments: `venue` (omit for every engine), `reason`.
3. Read `preview_token`. In an incident, `confirm: true` WITHOUT the token hands back another
   preview and does not complain — take the token out of the preview
   (`https://vike.io/docs/ai/trader/tools/market_exit`).
4. `market_exit` again with the same `venue` (or none), `confirm: true`,
   `preview_token: <token>`. The token is good for 60 s.
5. `node_snapshot` — check the board is flat. **It is best-effort, not atomic**: the mass-cancel is
   fire-and-forget over the adapter's actor thread and its ACKs can land after the flatten market
   orders are already on the wire, so a resting order can still fill after a flatten leg. If the
   board is not flat, take a fresh preview and re-issue the exit.
6. Then close the door: Procedure A (`set_trading_state` `halted`) so nothing re-opens risk.

What it expands into on the node (`https://vike.io/docs/ai/trader/tools/market_exit`): one `MassCancel` first,
then one `Flatten` per non-flat net (`BOTH`-side) position derived from the post-cancel account,
each a reduce-only market order for `|position|`. Hedge-mode `LONG`/`SHORT` legs are SKIPPED — on
a venue reporting only hedge buckets the mass-cancel runs but the flatten half mints nothing; those
legs need a reduce-only submit_order, which this skill does not cover.

It works from a halted core with no precondition — `market_exit` and `set_trading_state halted`
work in either order, so never set `active` just to escape a halt. One residual under `halted`: the
gate re-takes its verdict on the lot-rounded size, so a position sitting off the lot grid rounds its
own flatten leg up into a flip and that leg alone is denied with the reason `halted`. Armed
per-order notional or total-exposure caps still deny an oversized leg in any state.

## Procedure D — close one book only

1. `node_snapshot` — read the `(venue, symbol)` position; the preview cannot show its size.
2. `flatten` — preview call. Arguments: `venue` and `symbol` (both required), `reason`. There is no
   quantity argument: the size is `|position|`, resolved on the node at apply time, and it is a
   no-op when flat (`https://vike.io/docs/ai/trader/tools/flatten`).
3. `flatten` again with the same `venue`, `symbol`, `confirm: true`, `preview_token: <token>`.
4. `node_snapshot` — see the fill. On an unknown outcome, snapshot before retrying: a repeat
   against a flat book is a no-op, but a repeat issued while the first is in flight is not obviously
   one. `flatten` is admitted under `reducing` and `halted` and is never size-capped by the node's
   control limits.

## What a process stop leaves at the venue (tell the user before they stop)

From `https://vike.io/docs/trader/guides/stop-liquidate-restart`:

- A stop NEVER closes a position. The shutdown sweep (`cancel_orders_on_shutdown`) cancels resting
  orders only, and that flag is OFF by default — a default stop leaves both orders and positions at
  the venue with nothing running to manage them.
- A hard kill (`kill -9`, `taskkill`) runs no cancel sweep, no state save, no journal snapshot.
- So the safe sequence before a stop is Procedure B (or C if the user wants flat), then Procedure A,
  then the stop. Stopping the process is outside this tool surface (`systemctl stop`, or the
  `shutdown`/`quit` word on the daemon's TTY).

## Restart checklist

From `https://vike.io/docs/trader/guides/stop-liquidate-restart`. A restarted `vike-tradehub` recovers profile
mounts, runtime mounts, saved strategy state, and the HALT FILE (a halted node restarts halted until
the file is removed). It does NOT recover:

- the order and position book — engines start empty; reconciliation adopts venue state, and the
  daemon defaults its policy to `quarantine`, so drift is HELD for a confirm;
- orders you did not cancel — they rest at the venue as `UnknownOrder` divergences, quarantined;
- attribution of post-restart fills on pre-restart orders (they land in the residual row);
- the in-process halt — `Halted` lives on the engine, and the engine is new.

After the node is back:

1. `node_snapshot` — check `trading_state` per venue, open orders, positions, recent events. If the
   book at the venue is not what the user expects, stop here and report — do not re-enter.
2. If the node restarted `active` and the user wanted it held, Procedure A immediately.
3. Clear leftover resting orders with Procedure B if they were meant to be cancelled.
4. Only then, with the user's explicit go-ahead, `set_trading_state` `active` through the gate.
   Caveat: if a drawdown latch put the node in `reducing`, `active` re-arms it against the same
   running high-water mark, so a still-underwater account latches again on the next closed-bar
   sweep (`https://vike.io/docs/ai/trader/tools/set_trading_state`).

## Never trade the account by hand while a mount runs

A running mount cannot tell your order from a stranger's; anything at the venue nothing local
explains is an external divergence, and under `hybrid` a position drift is auto-applied to the local
book at the venue's average price with no operator in front of it. If the user must act at the venue
directly, `set_trading_state` `halted` first (Procedure A), and expect reconciliation to raise what
they did (`https://vike.io/docs/trader/guides/stop-liquidate-restart`).
